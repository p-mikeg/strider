# Changelog

## Unreleased

### Breaking, Python

- A `one_of` / `first_of` arm nests through the edge the alternation's slot
  takes, as Rust's does, so `one_of([x])` answers what `x` does in every
  operand slot. In a memory slot this is a silent change of answer:
  `store().mem(one_of([call().res()]))` and
  `load().mem(one_of([call().res()]))` matched nothing and now match the
  call's memory token, as `.mem(call().res())` does.
- A `call()` / `call_other()` in a raw slot (`input`, `any_input`,
  `phi_token`, `call_other().arg`) binds any of its outputs, its control,
  memory or value edge, as a lone `one_of` arm of one did, where it bound a
  value output alone. A silent change of answer:
  `region().any_input(call())`, `load().input(0, call())`,
  `call().input(0, call())`, `ret().any_input(call_other())` and
  `call_other().arg(0, call())` matched nothing and now match what their
  `one_of([...])` forms always have. A `.capture()` on such a call binds the
  edge it matched, so a rewrite reusing it as a value is refused; `.res()`
  keeps it to the value output.
- `entry()` and `region()` as a value operand (of a value op, or as an
  `addr`, `data`, `target`, `arg`, `cond`, `selector`, `ret_val` or
  `phi_input`) raise at query time, as in Rust, where the query returned
  `[]`: they produce only a control edge. `ValueLike` no longer lists them.
- A builder's `into_pat()` `Pat` holds the builder by reference rather than
  a compiled snapshot, so a later edit to the builder changes what an earlier
  `into_pat()` matches. It answers every query it is passed to, where it was
  used up by the first, nests as its builder does, and `.capture()` on it is
  the builder's own. A wrong-typed operand, or a wrong part in a memory slot,
  still raises at `into_pat()`; an operand that can never match its slot
  (`load(store())`, `call().arg(0, store())`) raises at the first query.
- `IfPat.true_branch` and `IfPat.false_branch` are gone. Join the edge's
  consumer on the edge capture instead: `if_else().true_branch(region())` is
  `fn.find_all([if_else().capture_true(t), region().any_input(t)])`, and a
  `call` / `ret` consumer takes it as `.ctrl(t)`. Each row is one `Match`
  whose `roots` are the `If` and the consumer.
- `Node.wide_const_bytes()` is gone. `Node.uint()` reads a constant at any
  declared width, a Python `int` being arbitrary-precision, so the bytes
  escape hatch had nothing left to carry.
- `Lifter.visualize` is gone: `strider.explore.visualize(target, *, host=,
  port=, depth=, whole=, background=)` is the one entry. It takes `host` and
  the rest by keyword only, and a negative `depth` raises `ValueError`, where
  `Lifter.visualize` raised `OverflowError`.
- `CallingConvention.no_return()` is gone. List a callee that never returns
  in `CfgOptions(no_return_targets=...)`, which `Lifter.build_cfg` honours
  too; it ignored a no-return `per_address_ccs` entry.
- `CfgOptions(function_max_size=n, allow_code_before_start_addr=True)` raises
  `ValueError`; the flag had no effect beside a size.
- `CfgOptions.with_function_max_size`, `CfgOptions.with_data_ranges` and
  `LifterOptions.with_cfg` are gone; pass the fields to the constructors.
  `ElfLifter.analyze(<name>)` still bounds the options by the symbol's size,
  which leaves `allow_code_before_start_addr` `False`.
- `Cfg.{isa_mode_conflicts, interior_branch_targets, unmapped_branch_targets,
  undecodable_branch_targets, unverified_seeded_sites}()` are gone:
  `Cfg.incomplete` lists every `(address, reason)`, with the reasons in
  `strider.cfg.INCOMPLETE_REASONS`. `AnalyzeResult.unresolved` is unchanged.
- `Cfg.is_complete()` is gone: test `not cfg.incomplete`. `analyze` and
  `build_cfg` return whatever it names and never raise over it, so the
  `OnIncomplete`, `IncompleteAnalysis` and `on_incomplete=` added since 0.2.0
  are gone too.
- `strider.opt.StackOffsetDetect` is gone: every run records the stack and
  heap slots after its post-passes, which is all the pass did.
- `Lifter.reg` and `Lifter.reg_name` are gone: `lifter.arch.reg(name)` and
  `lifter.arch.reg_name(vn)` answer from the same table.
- `lifter=` is gone from `Function.to_dot`, `Function.neighborhood_dot`,
  `Cfg.to_dot` and `Cfg.neighborhood_dot`, and with it the check that a
  supplied handle matched the graph's arch: a render reads the arch's name
  table, not a decoder, so it runs on any thread and while the owning
  `Lifter` is mid-`analyze`, where both raised.
- `strider.pattern.{int_cmp, int_binary, bool_binary, float_binary,
  int_extend}` and `strider.template.{int_cmp, int_extend}` take the op's own
  name only, the string `Match.op` and `Node.op()` read back: the aliases
  `"eq"`, `"lt"`, `"slt"`, `"shl"`, `"shr"`, `"sshr"`, any other case
  (`"add"`, `"EQUAL"`) and `int_extend`'s `"zero"` / `"sign"` /
  `"zero_extend"` / `"sign_extend"` raise `StriderError`, which lists the
  names; `int_extend` takes `"ZeroExtend"` / `"SignExtend"`. The `*OpName`
  Literals list those names alone.
- `Node.kind()`, and so `repr(Node)`, renders `IntConst`, `InitialVar` and
  `CallOther` bare, without the interner handle (`"IntConst(const12)"`,
  `"InitialVar(InitialVnId(6))"`, `"CallOther { user_op_id: 120 }"`); read
  the payload through `uint()` / `sint()`, `vn()` and `call_other_name()`.
- `BoundCapture` (`m[c]`) keeps `has`, `int()` / `operator.index`, `==` and
  its repr, and loses its reader properties: read a capture through the
  `Match`, `m.uint(c)`, `m.node(c)`, `m.op_opt(c)` and the rest. `m[c].uint`
  raises `AttributeError`.
- A `strider.pattern.Pat` is no longer a rewrite right-hand side:
  `Function.rewrite(replace=)` and `rewrite_all` raise `TypeError` for one,
  and one nested in a `strider.template` operand raises `StriderError`, a
  build-valid one (`var()`, `int_const()`) included. Both name its
  constructor and, where `strider.template` has a twin, point at it. Spell
  the RHS with `strider.template` (`template.var(c)`, or the bare
  `Capture`); the RHS `.capture(c)` goes with it, as a `Template` has none.
- `strider.sleigh.Sleigh` is gone: its `reg` / `reg_name` / `arch_name()`
  are `SleighArch.reg` / `reg_name` / `name()`, and a `sleigh=` other than a
  `SleighArch` raises `TypeError: argument 'sleigh': ... cannot be converted
  to 'SleighArch'`.
- `Lifter.optimize(function, pipeline=None, opts=None)` is
  `Function.optimize(assumptions=None, pipeline=None)`, run against the rom of
  the `Lifter` that produced the function, so the error for a function from
  another handle is gone. `pipeline` is second, so pass it by keyword:
  `function.optimize(pipe)` raises `TypeError`. It takes the claims to add as
  `assumptions=`, an `AssumptionOptions`, in place of `opts=`, whose other
  fields it ignored. It adds them to the function's and withdraws none.
  `assumptions=None` adds nothing, where `opts=None` applied the three default
  claims; a set that leaves out a claim already in force raises `StriderError`
  naming it, and each missing allocator address, before anything runs. Pass
  `assumptions=function.assumptions | AssumptionOptions.none(escape_analysis=True)`
  to add one; analyse again to drop one.
- `AssumptionOptions` is unhashable: `hash()`, set membership and dict keys
  raise `TypeError`. It compares by value (`function.assumptions ==
  AssumptionOptions.none()`), where it compared by identity.
- `Lifter.call_other_abi(name, opts)` checks `opts.call_other_abis` as
  `build_cfg` does and raises the same `StriderError` for an entry built
  against another arch's register table, where it returned that entry.
- `ElfLifter.arch` and `ElfLifter.reader()` are the base `Lifter`'s: both
  raise the re-entrant `StriderError` while an `analyze`, `build_cfg` or
  `add_elf` on the handle is in flight, and `reader()` returns the one
  `BufferReader` the handle decodes through, where each call built a new
  one over the same regions. `repr()` still answers meanwhile, without the
  arch, and `ElfLifter.endianness` reads the first ELF's `EI_DATA`, where
  it read the arch.
- `Function.to_dot(pretty=True)` names a `Load` / `Store` space by its sla
  name, where a space outside RAM, register, unique and const read `??`
  and an arch's default code space read `ram` whatever the sla calls it.
- Every integer reader is arbitrary-precision, at every declared width.
  `Node.sint()`, the `Match` readers (`sint` / `sint_opt`) and
  `find_unique_value(..., signed=True)` no longer answer `None` for a
  constant past 128 bits, where `None` could not be told from "no match";
  `BoundCapture.__int__`, `__index__` and `__repr__` follow.
- `BoundCapture` has no truth value: `bool(m[c])`, and so `if m[c]:`, raises
  `TypeError` naming `.has` and `int()`, where it was `True` for an unbound
  capture and for a zero constant alike.
- `BoundCapture.__eq__` compares at full width, so `m[c] == 2**300` answers
  `True` where it answered `False` for a matching `I512` constant. This is a
  silent change of answer, not a new error.
- `strider.pattern.int_const`, `strider.template.int_const` and a raw int
  pattern or template operand take a Python `int` of any size; one past 128
  bits failed to convert at all. An integer wider than every value type
  (512 bits) now raises `StriderError` naming its bit length, read off
  `int.bit_length()` before any conversion allocates. A comparison against
  one (`m[c] == 2**600`) answers unequal rather than raising.
- `ElfLifter.analyze` given an `opts` that is not a `LifterOptions`, and
  `ElfLifter.build_cfg` given one that is not a `CfgOptions`, raise
  `TypeError` naming the argument, as `Lifter.analyze` and `build_cfg` do.
  They raised `AttributeError` from reading the options' fields.
- `StriderError.backtrace` read off the class is a property, not `""`; an
  instance reads as before.
- `SleighArch`, `CallingConvention` and `Symbol` compare and hash by value, as
  `VnSpace`, `Vn` and `CallOtherAbi` do: `lift.arch == SleighArch.x86()`,
  `lift.cc == CallingConvention.x86_cdecl()` and two reads of one symbol
  answered `False`, each a fresh object. A `custom(...)` convention never equals
  a preset, nor one resolved against another arch.
- `bool()` of a `CastMask` is whether it selects any cast, so
  `if mask & CastMask.truncate():` tests membership. Every mask was true,
  `CastMask.none()` included.

### Breaking, Rust

- `TemplatePat` is sealed: no type outside strider-pattern implements it, so
  a hand-written type is no longer a rewrite RHS. `MatchPat` is still open.
  A front end that builds the tree itself reaches a template through the
  hidden `Expr::into_template` / `Expr::capture_typed`.
- The `*_const_with!` capture lists drop the `int_unary_op` kind: `Neg` is
  the one `IntUnaryOp`, so there was nothing for it to tell apart.
- A `*_const_with_fn` constant declares its captures one at a time,
  `.declare(c)` / `.declare_optional(c)`, where `.declaring(vec)` /
  `.declaring_optional(vec)` replaced the list.
- A repeated `CallOtherPat::name` or `PhiPat::for_vn` requires every name or
  varnode given, where the last one replaced the rest, so
  `phi_for(rbx).for_vn(rax)` matches nothing rather than the `rax` phi. A
  silent change of answer. Python's `.name()` / `.for_vn()` still replace.
  `IfPat` no longer implements `Default`; `if_else()` builds it.
- `WithOutput`'s `capture_output` / `output_width` / `output_ty` are one
  `with_output(slot, OutputAspect)`, `OutputAspect` being the one constraint
  an `.output(slot)` / `.any_output()` terminal commits.
- `MatcherBuilder` and `TemplateBuilder` build straight into the pattern
  graph, so `input` refuses a producer built after its consumer, reported
  through the root at `finish`: build each operand before the node consuming
  it. A template `capture` may still be built anywhere. A cyclic wiring is
  refused by the same rule, where the refusal named a cycle, and like any
  refusal it is reported only when it comes first: a cycle wired before an
  explicit `reject` is now the error reported, where the `reject` was.
- A `call()` / `call_other()` in a raw slot (`input`, `any_input`,
  `phi_token`, `CallOtherPat::arg`) nests through any of its outputs, its
  control, memory or value edge, where it nested through a value output
  alone, so `region().any_input(call())` and `load().input(0, call())` match
  a call's control and memory edges, as a Python `one_of` arm of one did. A
  `.capture()` on such a call binds that edge, so a template reusing it as a
  value is refused; `.res()` keeps the value-only binding. A memory, control
  or value slot is unchanged.
- `CfgOptions` gains `local_entries`, a ppc64 global entry's local entry,
  through which every per-address table answers a local-entry address; a
  literal naming every field passes `&strider_cfg::NO_LOCAL_ENTRIES` for
  none.
- `strider_ir::Function::raw_html` is gone: `raw_graph_dot()` returns the
  raw renderer, whose `as_html_from_dot()` renders the HTML
  (`Function::raw_dot()` is the DOT text).
- An operand that can never match its slot is refused through the pattern's
  `root()`, where the query matched nothing: in a value slot (a value op's
  operand, `float_le` / `float_is_nan` included, `target`, `arg`, `addr`,
  `data`, `cond`, `selector`, `ret_val`, `phi_input`) a `store` / `mem_phi`,
  an `entry` / `region`, or a `ret` / `if_else` / `switch` /
  `indirect_branch` / `unreachable`; in a `ctrl` slot a `store` / `mem_phi`
  or a `ret` / `indirect_branch` / `unreachable`; in a memory slot, which
  the typed builders gate at compile time and the hidden `IntoExpr` does
  not, a node with no memory token or a value op. Decorations are looked
  through, an alternation's arms are not, and the raw `input`, `any_input`,
  `phi_token` and `call_other().arg` slots take any edge as before.
- `MatchPat` and `TemplatePat` are markers over the hidden `IntoExpr`, which a
  hand-written match pattern implements in place of `compile`; what it lowers
  to is checked on the match side. `MatchPat::compile_mem` and
  `TemplatePat::compile` are gone, and `MatchPat::compile` is a provided
  method. `MatcherBuilder::set_output_control` and `set_token_fallback` set
  the one vertex they are given, where they recursed into an alternation's
  arms: a slot reaches each arm as it lowers.
- The typed pattern builders return two carriers, `Op<T>`, valid on whichever
  sides its operand types `T` are, and `MatchOnly`, valid on the match side
  alone, where each op returned its own struct. The re-exported `AnyBool`,
  `AnyBoolConst`, `AnyFloat`, `AnyFloatConst`, `AnyInt`, `AnyIntConst`,
  `IntConstAllOnes`, `IntConstAnyWidth`, `Guarded`, `Limited`, `OfWidth`,
  `Ordered`, `ValueTy` and `AltSlot` are gone; `Captured<P>` stays, a template
  at `P = Var` only. A factory's operands, and the pattern a `CaptureExt`
  method decorates, must be `'static`, `TemplatePat` requires `Send`, and a
  `when_match` / `filter` closure must be `Send` where it is passed rather
  than where the pattern is lowered.
- `IfPat::with_true` and `IfPat::with_false` are gone, and with them
  `BindingWalkFn`, `WalkCaptures`, `MatcherBuilder::set_binding_walk` and
  `Pattern::has_binding_walk`. Join the edge's consumer on the edge capture
  instead: `if_else().with_true(region().build())` is
  `find_joined_constrained(&[&if_else().capture_true(t).build(),
  &region().any_input(var(t)).build()], &[])`, and a `call` / `ret` consumer
  takes it as `.ctrl(var(t))`.
- `Match::from_root` and `Match::from_root_in` are gone:
  `Match::new(root, bindings, generation)` takes the producing graph's
  `Graph::generation`, so every `Match` is stamped and `is_stale` always
  checks.
- `matcher::OutputKindSpec` is crate-private, and its `PhiToken` arm is gone:
  nothing built such a vertex, and no public signature names the type.
- `strider_pattern::int_const` takes an unbounded signed value (`u8`..`u128`,
  `i8`..`i128`, `UBig`, `IBig`, or a collection of them), so a query names a
  constant of any declared width, and a negative wraps into whatever width it
  is matched at rather than into 128 bits. A template `int_const` of a
  negative wraps at the width it materialises at.
- `Bindings::get_int`, `get_bool`, `get_int_binary_op`, `get_int_unary_op`,
  `get_int_cmp_op` and `get_bool_binary_op` are gone: read the bound node's
  kind through `get_node`, an integer through `get_uint` (or `get_value` and
  `int_const_ibig`), and a boolean through `get_value` and `bool_const_val`.
- `strider_pattern::MissingBinding` is gone: `missing_binding` returns the
  same message as a plain `anyhow::Error`.
- `MatcherBuilder::unary`, `TemplateBuilder::unary` and
  `TemplateBuilder::binary` are gone: build the node with `node`, `input`
  and `value_output`.
- cranelift-entity is 0.128, where it was 0.112. It is a public dependency:
  `NodeId` and the other ids implement its `EntityRef`, so a crate on 0.112
  no longer type-checks against them.
- `DenseEntitySet` no longer has `len`, and `entity_utils::set::Iter` is
  gone: the set has `contains`, `insert`, `remove`, `is_empty`, `clear` and
  `iter` (cranelift's `SetIter`), plus `Extend`, `FromIterator` and
  `IntoIterator` for `&DenseEntitySet`.
- `graph_algorithms::walk::GraphRef::NodeId` is bounded by `EntityRef`, where
  it was `Copy`.
- `IRBuilderExt::build_int_const_limbs` is gone: `build_int_const` takes any
  `impl Into<UBig>`, so a limb spelling is `UBig::from_words`.
- `ValueType::get_unsigned_int` is gone; it had no caller.
  `strider_ir::eval::mask` is the unsigned read, at every width.
  `int_mask_u128`, `wrap_u128` and `get_signed_int` stay: the signed carrier
  is still an `i128` for the stack-offset and index arithmetic that reads it.
- A call output or return varnode outside register and unique space is
  refused naming it as the IR text spells it, with the space shortcuts
  (``r:0x1000:4, in space `r`, outside register (`%`) and unique (`u`)
  space``), where the message spelled the space and the offset apart.
- `DecodeEngine::new(arch, reader)`, `Lifter::new(arch, reader)` and
  `Strider::new(arch, reader, rom)` take the `MemReader` and build the arch's
  Sleigh themselves, through the new `SleighArch::sleigh(reader)`; none takes
  a `Sleigh`, so the error for one built from another sla or pspec is gone.
- `Optimizer::apply`, `PostOptimizer::apply`, `OptimizerPipeline::run`,
  `run_one`, `run_one_unvalidated` and `run_post` take `&OptCtx`, not
  `&mut OptCtx`: nothing writes it.
- `SideTables::set_frame_address_in_call_register` is
  `record_call_register_frame_reach`, taking the stack address's offset from
  the entry SP (`i128::MAX` for none); `call_register_frame_reach` reads the
  highest recorded.
- `BuiltCallingConvention` has a `reg_args_adjoin_stack_args` field (on for
  the ARM AAPCS, MIPS O32 / N64 and PowerPC64 ELF presets), so a struct
  literal names it; `incoming_args_start` is where the incoming arguments'
  memory starts, register homes included. The text header prints it when set.
- `Lifter::build_ir_with` takes the claims the lifted function holds, which
  the lift reads (`msr_register_keeps_mode`); `build_ir` lifts under none.
- `template::instantiate` refuses what it built at the root's width: a fresh
  untyped operand at a slot that owes its node no width (a `Popcount` /
  `Lzcount` operand, a `Load` / `Store` address or data, a `Phi`, `Call` or
  `Return` input, a conversion's operand), an untyped output no template node
  consumes, an output typed from a capture that bound no typed value, and a
  node that declares no output. Type it with `set_value_ty` or
  `capture_typed`. A shift count takes the shift's width.
- `BuiltCallingConvention::no_return` is gone, replaced by
  `CfgOptions::no_return_targets`, so `Strider::build_cfg` ends a call to one
  as `analyze` does. `Lifter::build_cfg` loses its `per_address_ccs` argument
  and `Builder::with_per_address_ccs` is gone: the CFG reads no calling
  convention.
- `to_text` drops `no_return=` from the `default_cc` line and from a call's
  override, so a call to a no-return target prints as a plain `call`.
- `CfgOptions::{fn_max_size, allow_code_before_start_addr}` become
  `bound: FunctionBound`, either `Size(NonZeroU64)` (`FunctionBound::size(n)`,
  which panics on 0) or `Open { code_before_start }`. `Some(0)` used to read
  as unbounded, and the flag was ignored beside a size.
- `LiftOptions` is gone. `Strider::analyze(entry, cc, &AnalyzeOptions,
  pipeline)` takes `AnalyzeOptions { cfg, per_address_ccs,
  resolve_indirect_branches, assumptions, compact }` in place of
  `LiftOptions` and `OptOptions`, and
  `Lifter::build_ir_with(cfg, cc, &cfg_opts, &per_address_ccs, &claims)` takes
  the `CfgOptions` the cfg was built with.
- `AnalyzeResult`'s six report fields and `Cfg::{isa_mode_conflicts,
  interior_branch_targets, unmapped_branch_targets, undecodable_branch_targets}`
  are gone: `AnalyzeResult::incomplete()` and `Cfg::incomplete()` return
  one `Incompleteness` of `(PcodeInsnAddr, Reason)`, read per reason with
  `of(reason)`; `is_empty()` is "complete". A `build_cfg` CFG also names its
  unresolved and seated sites.
- `AnalyzeResult::is_complete()` is gone: test `incomplete().is_empty()`.
  `Strider::analyze` and `Strider::build_cfg` return whatever it names, so
  the `AnalyzeError`, `OnIncomplete` and `AnalyzeOptions::on_incomplete`
  added since 0.2.0 are gone too, and both return `anyhow::Result`.
- `CfgOptions` gains `derived_targets`, a resolver's own answers, which
  beat `known_targets` at their exact key; a literal naming every field
  passes `&strider_cfg::NO_TARGETS` for none. `build_cfg` seats them;
  `Strider::analyze` fills that layer itself and returns `Err` on a
  caller's non-empty one.
- `CfgOptions::call_other_overrides` is borrowed, as `known_targets` is, so
  the resolve loop's per-round clone no longer copies it; a literal naming
  every field passes `&strider_target::call_other_abi::NO_OVERRIDES` for none.
- `IndirectBranchClassify`, `OptCtx::indirect_resolutions` and
  `OptimizerPipeline::has_post_pass` are gone, and `PostOptimizer` no longer
  requires `Any`: `analyze` classifies every round after its pipeline,
  whatever the pipeline, with `strider_opt::classify_sites(&function, rom)`,
  which returns a `ClassifiedSites` and reads the claims off `function`.
  With `resolve_indirect_branches` off it calls no classifier and lists the
  sites unresolved (`ClassifiedSites::unresolved(&function)`).
- `ResolvedTargets::Multiple` holds `Arms`, non-empty and sorted by
  `(addr, isa_bit)` with no exact duplicate, built by
  `ResolvedTargets::table`; `table([])` is the new `Withdrawn`, which seats
  no arm and defers its site. `arms()`, `successors()` and `narrows_to()`
  read any variant.
- `strider_opt::apply_rules_count_with` is gone: `apply_rules_count`
  re-derives the memory classes under the claims the function holds, where
  it used none and dropped every heap class.
- `SideTables::clear_memory_slots` is gone: `invalidate_analyses` drops the
  memory classes and the frame-escape verdict together.
- `SideTables` is no longer `Default`: only a `Function` builds one, so
  nothing can reset its claims to `none()`.
- `SleighArch::probe_regs` (`test-util`) returns `SleighRegs`, not a
  `Result`: it copies the table `SleighArch::names` holds.
- `StackOffsetDetect` is gone: a converged run stamps the memory classes
  after its post-passes, and a post-pass that reads them before that calls
  `refresh_memory_classes`.
- `value_range::Interval`'s fields are private (`Interval::new`, `lo()`,
  `hi()`, `stride()`, `is_empty()`); `compute_value_ranges(f, &doms, &known)`
  is `RangeMap::new(f)`, which builds both itself and returns `Result`.
- `EditFunction::replace_all_uses` is private: it left the old producer live
  and dropped its asm-fingerprint. `replace_value` does the same rewire and
  handles both.
- `EditFunction::replace_value` returns `bool`, not `Result<bool>`, and
  `add_node_input` and `remove_region_predecessors` return nothing, not
  `Result<()>`: none of the three could fail.
- `ValueType::{int_for_byte_size, float_for_byte_size}` are renamed
  `int_from_bytes` / `float_from_bytes`.
- `ValueType::ALL` is renamed `SUPPORTED`, and `ValueType::as_str` is gone:
  `Display` spells the same `i32` / `f64`, and `ValueType` now parses that
  spelling (`FromStr`).
- `ValueType` is `enum { Int(u16), Float(u16) }`, the width in bits. Only
  `ValueType::I1` keeps a name: write `ValueType::int(32)` /
  `ValueType::float(64)` (`const fn`s that panic, or fail to compile in a
  `const`, on a width outside `SUPPORTED`; `is_supported` tests one) and
  match `Int(w)` / `Float(w)`. `Debug` and `Display` print `I32` / `i32` as
  before; `float_from_bytes`' error no longer lists the valid sizes, and
  `int_from_bytes`' reads `unsupported integer varnode size`.
- `ValueType::bit_mask_u128` is gone: it saturated to `u128::MAX` past 128
  bits. `wrap_u128(v)` reduces a value to the width, exactly at every width
  (a float gives 0); `int_mask_u128()` is the all-ones value, `None` for a
  float or past 128 bits.
- `ValidationError::UnsupportedValueType` is new: `validate` rejects a value
  typed with a width outside `ValueType::SUPPORTED`, which only the `Int` /
  `Float` variants can write.
- `template::instantiate` checks every node's input count and input kinds
  against its node signature. Before, no node's input count was checked, nor
  any input of a raw `Load`, `Store`, `Phi`, `MemPhi`, `New` or `CPoolRef`,
  so a template could build IR `validate` rejects.
- `build_store`, `build_load`, `build_cpool_ref` and `build_new` return `Err`
  on a float address, data or `CPoolRef` operand, and on a float output for
  `Load`, `CPoolRef` or `New`; each checks its node signature.
  `build_int_binary_operation`, `build_int_unary_operation`,
  `build_int_cmp_operation`, `build_float_binary_op` and
  `build_float_unary_op` check theirs too: an int op over floats, or a float
  op over ints, returns `Err` where `validate` used to reject it later.
- `strider_target::FlowCommit` and `SleighArch::commits_flow_context` are
  gone, and `SleighArch::flow_commit_vars` lists names alone: each var's flow
  class is the sla's own `noflow` declaration, read off by the decoder.
- `strider_cfg::{Builder, FlowVars, FlowContext}` are private, and with them
  `Builder::for_arch` and its `with_*` setters. Build a CFG with
  `DecodeEngine::new(arch, reader)?.build_cfg(entry, &opts)`, which pins the
  entry's ISA mode and carries the decode context across builds; a bare
  `Builder` decoded ARM with ISA-mode handling off.
- `graph_algorithms::dominance::{DomRelation, DomTree, DomTreeWalk,
  dominator_tree_walk, dominator_tree_preorder}` are gone.
  `Dominators::intervals` and `Dominators::tree_preorder` (siblings ascending
  by vertex) number the tree, and `dominance_frontiers(&doms, preds)` takes
  the predecessor edges as a closure.
- `Function::new` keeps only the largest of nested varnodes, as
  `FunctionBuilder::new` did; it only sorted. `Function::set_all_vns`
  (test-util) is gone.
- `ContainerMap` keeps the tracked set it was built over, so `container_of`
  loses its slice argument, and it no longer implements `Default`.
- `ElfFileMemReader::from_object` and `elf::elf_get_loadable_regions` are
  gone, reversing 0.2.0's copying constructor, which cost 312 MB of RSS and
  94 ms on a 124 MB `.so`. `ElfFileMemReader::new(&elf,
  RelocationMode::Unapplied)` windows an `OwnedElf`'s bytes; a caller holding
  only an `object::File` or a byte slice builds one with `OwnedElf::parse`.
- `elf::without_ranges` takes an `&elf::AddressSet` instead of a range slice.
- `OwnedElf::writable_ranges` is gone: `OwnedElf::image(..).writable` is the
  same set as an `elf::AddressSet` (`as_slice` for the ranges), from the parse
  that builds the regions. `image(.., RelocationMode::Applied { .. })`
  relocates too, so it can fail where `writable_ranges` did not.
- `Function::dot_dumper`, `Cfg::dot_dumper`, `Cfg::neighborhood_dot` and
  `strider_cfg::insn_text` take `&ArchNames` (`arch.names()`) instead of a
  `Sleigh` (and, for `insn_text`, its register table); `FunctionDotDumper`
  and `CfgDotDumper` lose their reader parameter, and `Function::dot_dumper`
  no longer returns a `Result`.
- `graph_algorithms::walk::GraphRef` has one method,
  `successors(node, f: impl FnMut(NodeId))`: `try_successors` and its
  `ControlFlow` early exit are gone, as is `walk::entity_postorder` (call
  `PostOrder::new`).
- `strider_graph::Vertex` and the petgraph traits on `strider_graph::Graph`
  (`GraphBase`, `IntoNeighbors`, `IntoNeighborsDirected`,
  `IntoNodeIdentifiers`, `Visitable`) are gone, and strider-graph no longer
  depends on petgraph: walk a graph with `graph_algorithms::walk` over a
  `GraphRef` adapter of your own (`Graph` implements none) whose `successors`
  lists a node's input producers or its outputs' consumers.
- `strider_opt::{value_range, indirect_branch_resolve, KnownBitsFacts,
  KnownBitsMap, analyze_known_bits, classify_target}` exist only with the
  `test-util` feature: only tests read them. `classify_sites`,
  `ClassifiedSites`, `ReturnTargets` and the `KnownBits` pass stay public.
- `KindSpec::VariantWith` is gone. `load().space(s)`, `store().space(s)` and
  `call_other().user_op_id(v)` pin `KindSpec::Exact`, the same test on
  kinds whose payload is that one field, so two such operands of a
  commutative node now count as interchangeable.
- `TemplateBuilder::{leaf, node}` take a `NodeKind` instead of a
  `KindSpec`, and `set_template_kind` is gone: `dynamic_leaf(TemplateKind)`
  builds an input-less leaf whose kind or constant a closure computes, so a
  closure-kinded node can no longer take inputs. A non-exact spec became an
  `IntConst` with the target function's constant id 0.
- `typed::builder_like` (`BuilderLike`, `CompileInto`) is gone.
- `PatNode::input_slots` holds `matcher::InputSlot` (`At(slot)` or `Any`)
  instead of a `usize` with `usize::MAX` marking an existential.
- `PatValue::{match_slot, any_slot}` become `slot_mode: matcher::SlotMode`
  (`Loose`, `Pinned`, `Enumerated`), and
  `MatcherBuilder::set_value_out_slot(out, slot)` becomes
  `pin_value_out_slot(out)`, which pins the vertex's own slot; create the
  vertex at the slot you pinned.
- `Match::bindings_clone` is gone: it was `m.bindings().clone()`.
- `OwnedElf::regions` and `strider_reader::elf::LoadFilter` are test-only
  (`test-util` feature), and `strider_reader::elf::sections` is private (its
  public items stay re-exported under `elf::`): `OwnedElf::image` is the one
  way to cut an image's fetch and ROM region sets.
- `IRViewer` loses the builders' operand checks: `require_value_type`,
  `require_value_kind`, `require_bool_value`, `require_phi_token_kind`,
  `require_control_kind`, `require_memory_kind`, `require_integer_value`,
  `require_float_value`, `require_integer_type`, `require_float_type` and
  `validate_value_inputs` are crate-private; each still runs inside the
  builder that needs it.
- `ReturnTargets::new(function)` and `classify_target(function, branch,
  rom, ranges)` lose their `AssumptionOptions` argument: both read
  `Function::assumptions`, the claims the function was optimized under.
- `OptOptions` is gone: `resolve_indirect_branches` and `assumptions` are
  `AnalyzeOptions` fields, with the same defaults, and each round claims
  `assumptions` on its fresh lift before the pipeline runs.
- `OptCtx` is `{ rom }`, built by `OptCtx::new(rom)`, and carries no claim;
  `OptCtx::at_floor` is gone. Claims accumulate per `Function`:
  `Function::assume(&claims)` adds to `Function::assumptions`, which every
  pass reads, and nothing withdraws one; analyse again for a function without
  one. A function never given a claim, a fresh lift included, is optimized at
  the sound floor, where a run under `OptCtx::new` used the three default
  claims.
- `RegionIndex::overlapping` is gone; `RegionIndex::covering` remains.
- `strider_ir::Value`, the second name for `strider_ir::node::ValueId`, is
  gone: use `ValueId`.
- Crate-private now, having no user outside strider-ir:
  `FunctionBuilder::build_entry` (renamed `build_initial_memory`; a second
  call minted a second `InitialMemory`; `FunctionBuilder::new` already mints
  the one), `write_variable_from_id` (use `write_variable` with the varnode),
  `Function::vn_ids` (use `all_vns`, in the same order),
  `GraphWalkInfo::postorder` (use `reverse_postorder`, reversed) and the walk
  types `DefUsePostorder`, `RawDefUseSuccs` and `DefUseSuccs` (no
  replacement).
- `Cfg::function_isa_bit` is gone: `flowing_isa_bit_at_site` answers every
  indirect site on an arch with an ISA-mode var, already degrading to the
  function's own mode where the branch address carries no readable context.

### Changed, Python

- A `.when()` predicate no longer runs on a candidate whose other operands'
  node kinds already rule it out, so a predicate that raised there no longer
  aborts the query, and a side-effecting one sees fewer calls. The matches
  are unchanged.
- Assigning `LifterOptions.cfg` or `LifterOptions.assumptions` still raises
  `AttributeError`, now reading "readonly attribute" where it read
  "attribute ... is not writable": the option classes are frozen.
- A pattern compile's stack budget, a quarter of the thread's remaining stack,
  is read through `stacker` on every platform it reports a stack on. Windows,
  the BSDs, Solaris, illumos and Haiku, which took the fixed 48 KiB fallback,
  now budget from the real stack, so a deeper pattern compiles there; Android,
  which `stacker` does not report, now takes the fallback. Linux and macOS are
  unchanged.
- Of several captures a rewrite's template references and its pattern leaves
  unbound, the error names the first written, where it named the last.
- `hash(CastMask)` returns different values than in 0.2.0; equal masks still
  hash equal.
- A memory slot (`mem`, `MemPhiPat.phi_input`) takes `anything()`,
  `var(c)` and a bare `Capture`, as Rust does, which bind the memory token,
  through a `.capture()` or `.when()` as a `one_of` there was taken. A
  `.of_width()`, `.value_ty()` or `.bool_valued()` there, which can never
  hold of a token, raises, over a `one_of` too, where it matched nothing.
- Every typed builder nests in every match slot but a memory one, as its Rust
  twin does, so `region().any_input(if_else())` and `load().input(0, store())`
  match. One that can never match where it sits (a `store()` or `ret()` as a
  value operand) is refused at query time with Rust's reason.
- A refusal names the value-op constructor called (`int_add()`,
  `int_truncate()`) where it named the family (`int_binary()`, `a cast`).
  The nesting bound counts one level per nested operand, where a nested
  builder call cost two, and its refusal no longer says so. A rewrite's
  `find` given something that is no pattern raises a `TypeError` reading
  `expected a pattern (...)`; `find_all` raises one carrying that text inside
  pyo3's `PatQuery` extraction message.
- Under `stack_global_disjoint`, `LoadForward` moves a stack load's memory
  edge past a constant-address store, and a constant-address (GOT) load's
  past a stack store. It moves a load's edge past a store through another
  `noalias_allocators` result, and still through no call a claim makes
  transparent. A jump-table index reloaded past a global write is bounded by
  the check on the first load.
- `FunctionArgDetect` moves an argument load's memory edge under the same
  claims as `LoadForward`: past a constant-address store under
  `stack_global_disjoint` and a store through a `noalias_allocators` result,
  and through no call a claim makes transparent. In a pipeline without
  `LoadForward`, two reads of one stack argument with such a store between
  them are one carrier. Both passes' narrowing now rests on those two claims,
  which reverses the 0.2.0 entry that they rewire "only on structural facts".
- With `return_slots_survive_untracked_writes` cleared, return-target
  verification passes a constant-address store under `stack_global_disjoint`
  and a store through a `noalias_allocators` result, where it took both to
  clobber the return slot.
- `ElfLifter.build_cfg` derives its options from the ELF as `analyze` does:
  the `$d` mapping symbols seat `data_ranges` unless the options name some,
  and a ppc64 ELFv2 `no_return_targets` address also covers its
  `Symbol.local_entry`.
- `Function.to_text()`'s `assumptions` header line ends in
  `msr_register_keeps_mode` under the default claims.
- `Function.to_text()` names a REGISTER-space byte order unlike the data
  order: the header reads `endian big register=little` on `arm_be_kernel`, so
  a parser of the header must accept the ` register=little` suffix.
- `analyze` and `build_cfg` no longer fail when decoding falls through past
  `function_max_size`: the fall-through is a tail call to the address there,
  which `cfg.incomplete` names `"past_function_end"`, as it does a conditional
  branch's not-taken path past the end, a conditional instruction skipping
  itself there, and the return address of a call that still ends its path
  there (not one to a `no_return_targets` entry). A seeded jump-table arm
  whose code falls through past the end is dropped and named there.
- Two `BoundCapture`s with no integer value (any binding but an integer
  constant) bound to the same value of one function compare equal; every such
  pair compared unequal before, `m[c] == m[c]` included. An integer constant
  still compares by its value in either spelling, and an unbound capture
  equals nothing.
- `build_cfg` reports a seed seated past p-code op 0 (a memory-indirect `jmp`)
  as `"unverified_seed"`; it went unreported.
- `load()` / `store()` region filters apply in call order, the last winning:
  `store().heap_only().stack_only()` matched the heap store.
- `function_arg_reg` / `source_register` match a sub-register of the carrier
  (`EDI`, `XMM0_Qa`); they matched nothing.
- `analyze` refuses a calling convention whose stack geometry belongs to
  another arch (`x86_cdecl` on x86-64, ppc32 SysV on ppc64), including the
  header's convention under `load_elf(arch=...)`.
- `unresolved` names each machine address once, `Cfg.incomplete` once per
  reason.
- A post-pass that rewrites the graph drops the stack-slot stamps it may have
  invalidated.
- An unknown op name in `int_cmp`, `int_binary`, `bool_binary`,
  `float_binary` or `int_extend` lists the accepted names in its error.
- A pass added from Python carries its own type name in Rust panics and debug
  output.
- An unknown type name passed to `value_ty()` / `of_type()` raises
  `unknown value type "<name>": expected one of i1, ...`; it read `unknown
  output type`.
- A rewrite template's node-signature error spells a typed value as its type
  (`i64`), not `Typed(I64)`.
- `Cfg.region_at` answers the region with the greatest start where regions
  overlap, the rule the CFG builds by; it answered the lowest index.
- `load_elf` and `ElfLifter.add_elf` build an image from one parse and one
  walk per view, where they parsed and walked it three times. With
  `apply_relocations=False` they no longer read the writable mappings they do
  not load, so an image whose dropped writable mapping cannot be read loads
  instead of raising; the `relocated_got_is_rom` refusal comes before any walk.
- `load_elf` and `add_elf` see the region and relocation changes under
  Changed, Rust and Fixed: a crafted image's read-only pieces left at one
  start collapse to the later, and with `apply_relocations=True` no image
  fails with "patch budget", a GOT-base or TOC-pointer word no longer needs
  its symbol, an image-relative word whose addend does not fit it is a hole
  or wrapped, an `R_PPC64_REL32` past +-2 GiB is a hole, a mips64el
  `SHT_REL` relocation is dispatched by its real type (so a mips64el PIE
  serves bytes `[0,4)`), and a symbol-reading relocation against `STN_UNDEF`
  is a hole.
- `rewrite` / `rewrite_all` refuse a `replace` of any other type with a
  `TypeError` naming what they take, where PyO3's text listed the binding's
  internal variants.
- `replace="_"` / `"any_"` raises `cannot use "_" as a rewrite RHS: it is the
  wildcard, not a capture name`, naming the string passed; it named `any_`.
- `CallingConvention.custom` raises at construction for stack geometry that
  does not fit its arch: a stack pointer not as wide as its addresses, a
  stack-argument stride other than that width, or with no link register a
  `ret_stack_pop` below it (the first two raised at `analyze`). These take
  the Rust wording (see Changed, Rust): "stack pointer" where they said
  `stack_vn`, and ``BuiltCallingConvention: ret_stack_pop N is less than the
  M-byte return address a `call` pushes with no link register`` where the
  message spelled out x86's 4 and x86-64's 8 and that a link-register arch
  (ARM, MIPS, PPC) pops 0 but sets `link_register`. An unknown register name
  in either `custom` names its list: `CallingConvention.custom:
  arg_passing_regs: unknown sleigh register name "X"`.
- A `Lifter` works on any thread: `analyze`, `build_cfg`, `Function.optimize`,
  `pcode_at`, `call_other_abi`, `user_op_names` and `add_elf` no longer raise
  off the thread that built it, reversing 0.2.0. Each of them, a plain
  `Lifter`'s `arch` and `reader()`, and `rom()` on any `Lifter` raises
  `StriderError` ("already in use") while an `analyze`, `build_cfg` or
  `add_elf` is in flight on the handle, from a `read()` callback or another
  thread, and those three raise while any of these calls is; the others may
  overlap each other. All but `analyze`, `build_cfg` and `Function.optimize`
  raised PyO3's `RuntimeError`. Such an `add_elf` merges nothing.
- `AssumptionOptions(...)` takes its defaults from Rust: a field passed
  `None` takes its default (it raised `TypeError`), `noalias_allocators`
  reads back sorted and deduplicated, `repr` included (it kept the order and
  duplicates given), and on Python 3.10+ the constructor's signature shows
  `None` for every default.
- `Function.to_text()` prints a `frame_address_in_call_register` line after
  `default_cc` when a call's clobbered register held a stack address.
- `Function.to_text()` prints an `assumptions` line after `default_cc`, and
  before `frame_address_in_call_register`, naming the claims in force that
  are on (`Function.assumptions`), the allocators as sorted hex addresses.
- `CfgOptions(call_other_abis=...)` is resolved on first use, rebuilt only
  when the arch changes, and borrowed by every later `analyze` and
  `build_cfg` through it, where each call rebuilt it.

### Added, Python

- `AssumptionOptions.fp_rounding_mode_is_nearest` (default `True`): the x86
  SSE rounding mode, MXCSR.RC, the x87 RC at `fst` and `fstp` to memory, the
  PowerPC FPSCR.RN, the ARM FPSCR.RMode, the AArch64 FPCR.RMode and the MIPS
  FCSR.RM are round to nearest even at every conversion that rounds by them
  (`cvtsd2si`, `cvtss2si`, `cvtpd2dq`, `cvtps2dq`, `cvtpd2pi`, `cvtps2pi`,
  `cvtsd2ss`, `cvtpd2ps`, `cvtdq2ps`, `cvtpi2ps`, `cvtsi2ss`, the 64-bit
  `cvtsi2sd` and the scalar VEX forms; `fctid`, `fctiw`, `fctidu`, `fctiwu`,
  `fcfid*`, `frsp`, `xscvdpspn`; ARM `vcvtr`, `vcvt.f32.f64`, `vcvt.f32.s32`,
  `vcvt.f32.u32`, `vcvtb`, `vcvtt`; AArch64 `fcvt` to a narrower float,
  `fcvtn`, `scvtf`, `ucvtf`, `bfcvt`; MIPS `cvt.w`, `cvt.l`, `cvt.s`,
  `cvt.d.l`). Off, each one reads the mode and selects among nearest even,
  down, up and toward zero, so a mode `ldmxcsr`, `fldcw`, `mtfsfi`, `vmsr` or
  `ctc1` sets folds; an AArch64 FPCR read is volatile, so `msr fpcr` does not.
  A function holding an ARM `vcvtr` or VFP narrowing or integer-to-single
  conversion, a MIPS `cvt` or an x87 `fst` to memory tracks the mode register
  the conversion names (FPSCR, FCSR, the x87 control word) whatever the claim,
  which shifts the `InitialVnId`s after it; an AArch64 one tracks FPCR only
  with the claim off. `AssumptionOptions.none()` clears it; the lift reads it,
  so `optimize` refuses to add it. Before, the SSE forms rounded to nearest
  even and `none()` did not lift that premise. MXCSR.FTZ and DAZ are still
  taken as 0. A function holding a rounding SSE conversion now tracks MXCSR
  (and a PowerPC one the RN bits), which shifts the `InitialVnId`s after it.
- `CallingConvention.custom(reg_args_adjoin_stack_args=True)` declares that
  the argument registers have stack homes just below the stack arguments, as
  the ARM, MIPS and PowerPC64 presets do; a custom convention for such an ABI
  (riscv and Win64 too) must set it.
- `"past_function_end"` in `strider.cfg.INCOMPLETE_REASONS`: where code fell
  through past `function_max_size`.
- `strider.template.int_ne`, `int_le`, `int_sle` and `float_ne` build the
  lifter's `Xor(Equal(l, r), 1)`, `Xor(Less(r, l), 1)`, `Xor(Sless(r, l), 1)`
  and `Xor(FloatEqual(l, r), 1)`.
- `CallingConvention.custom` and `CallOtherAbi.custom` take a `SleighArch`
  as their first (`sleigh=`) argument; names resolve through one table per
  arch per process. `SleighArch.reg(name)` and `SleighArch.reg_name(vn)`
  look registers up without a lifter.
- `.ordered()` on `inputs_of_width(n, x)` and `bool_inputs(x)` pins what it
  pins on `x`, as in Rust, when `x` is a pattern; it raised. Over a typed
  builder (`int_binary`, `bool_binary`, `float_binary`) it still raises.
- `Function.assumptions` reads the claims in force on a function, as an
  `AssumptionOptions`: the ones `analyze` ran under plus every set an
  `optimize` added. `AssumptionOptions.union(other)`, or `a | b`, holds every
  claim of both.
- `AssumptionOptions.none(...)` takes the constructor's keywords and sets
  only the claims named, so `none(escape_analysis=True)` is one claim where
  `AssumptionOptions(escape_analysis=True)` is six.
- `AssumptionOptions.msr_register_keeps_mode`, default `True`: the mode an
  ARM register-form `msr` writes to the CPSR control field (`msr cpsr_c,
  rN`) is the mode already in force, so it re-banks nothing and leaves the
  mode alone; its masks and flags still update, and immediate `msr` and
  `cps` still switch. `AssumptionOptions.none()` clears it, which re-banks
  unless the mode is provably the current one. Wrong for a new mode built in
  a register (Linux's boot-time `safe_svcmode_maskall`, the switch into FIQ
  in `fiqasm.S`) and for a saved mode restored after a switch made since the
  save (the return to SVC in `fiqasm.S`'s `__set_fiq_regs`). The lift reads
  it, so `Function.optimize` raises `StriderError` on a set that adds it.
- `strider.template.capture_typed(c, inner)` builds `inner` at the width of
  the value `c` bound, as Rust's `capture_typed` does: the way to type a
  constant nothing else gives a width.
- `Symbol.local_entry`: where a ppc64 ELFv2 call sharing the function's TOC
  enters it, `address` plus the `st_other` local-entry offset, else
  `address` (keeping a Thumb bit).
- `strider.cfg.DOT_STYLES`, every theme name a render accepts; `DotStyle`
  is its `Literal`.

### Changed, Rust

- A `.filter()` node predicate also runs in the operand gate before each
  match attempt, so it runs more often per candidate, and an operand's
  predicate may run before its root's.
- An alternation root over several node kinds walks the graph once, where it
  walked once per kind; Python's `find_all(one_of([...]))` over mixed kinds
  included.
- The crate-root value-op factories (`int_add`, `int_cmp`, `float_neg`, ...)
  and `template::`'s are one set: `template::int_add` is `int_add`. An op
  takes any pattern operands and is a match pattern when they are, a template
  when they are, so a wrong-side operand is refused where the op is used
  rather than where it is built.
- `Template::referenced_captures` lists captures in written order, so a
  rewrite's coverage error and `instantiate`'s unbound-capture error name the
  first unbound capture written, where they named the last.
- `rewrite_rule_runtime` errors, and `rewrite_rule` panics, on an RHS
  `Template` with no root, whether the builder refused it or it is rootless
  or multi-sink, as both already did on such an LHS. The rule was built and
  then errored on every firing.
- Integer constants are stored as `dashu-int`'s `UBig` and evaluated at
  their declared width, so constant folding no longer stops at 128 bits: an
  `I256` or `I512` constant folds where it stayed an unfolded node, through
  arithmetic, the comparison and overflow arms, `Truncate` / `Extend` and
  `Popcount` / `Lzcount`. `Sdiv` / `Srem` still decline `INT_MIN / -1` and
  division by zero, and every operation still declines a float type.
- The known-bits lattice records facts for `I256` / `I512` values, so they
  fold and their known-zero bits reach the masked-phi collapse.
- `DecodeEngine::build_cfg`, and so `Strider::analyze`, no longer fail when
  decoding falls through past a `FunctionBound::Size` end: the region
  tail-calls the address there, reported as `Reason::PastFunctionEnd`, as are
  a conditional branch's not-taken path past the end, a conditional
  instruction skipping itself there, and the return address of a call that
  still ends `NoReturn` there (not one to a `no_return_targets` entry). A
  seeded arm whose code falls through past the end is dropped and named there.
- `FunctionArgPat::source` matches a sub-register of the carrier, as
  `function_arg_reg` does in Python.
- `Lifter` refuses a calling convention whose stack geometry belongs to
  another arch, so `Strider::analyze` does too.
- A seed naming one address in both ISA modes decodes it in base mode first,
  not in the order listed.
- `LoadForward` is a unit struct again (`Copy`); `LoadForward::default()`
  still builds one.
- `Lifter::build_ir_with` ends a `CallOther` exactly where the `Cfg` ended
  its region instead of re-deciding under the `cfg_opts` it is handed, so a
  CFG built under other `call_other_overrides` lifts as it was built (the
  op's footprint still comes from `cfg_opts`).
- A `CALL` whose target operand is not in the code space selects no
  `per_address_ccs` override; the CFG's no-return rule already ignored it.
- `Lifter::new`, `Strider::new` and `DecodeEngine::new` return `Err` when the
  arch and sla disagree on the ISA mode: a mode var the sla does not flow, or
  (not `DecodeEngine::new`) an unpaired `ISAModeSwitch`. Before, only a debug
  build asserted it.
- A relocated image no longer fails to load with "patch budget": every
  region shares one patch list, so a site any number of overlapping
  mappings cover is patched in all of them at no extra cost.
- A GOT-base or TOC-pointer relocation (`R_386_GOTPC`, `R_X86_64_GOTPC32` /
  `GOTPC64`, `R_ARM_GOTPC`, `R_PPC64_TOC`), whose value does not read the
  symbol, no longer needs the symbol to resolve.
- An image-relative word whose RELA addend its field cannot hold under the
  image's address width is no longer truncated or written whole. A MIPS64
  `R_MIPS_REL32` against STN_UNDEF with no `R_MIPS_64` composed whose addend
  fits neither 32-bit signedness is a hole, and so is such an `R_*_RELATIVE`
  / `IRELATIVE` in a crafted ELF64 `EM_386` / `EM_ARM` / `EM_PPC` image; a
  crafted ELF32 `EM_PPC64` `SHT_CREL` `R_PPC64_RELATIVE` is wrapped to 32
  bits.
- `ElfFileMemReader` holds the file's mapping: a read past the end of a file
  truncated under a live reader, or a paging error on a network or 9p mount,
  is a SIGBUS, as through any mapped `OwnedElf` (an `OwnedElf::parse` one owns
  its bytes); `STRIDER_NO_MMAP=1` reads the file instead. A load no longer
  fails with "distinct file bytes": nothing is copied.
- `ElfFileMemReader`'s `ReadOnlyMemory` view is the image's immutable
  mappings, built once, rather than the fetch image minus its writable ranges
  at every read; a read of a writable address errors as unmapped ("not
  mapped", or "spans past mapped memory" for a read running into one), where
  it said "is in a writable mapping". Two crafted shapes serve other bytes:
  where a writable mapping cuts two overlapping read-only mappings into
  pieces at one start, the ROM view serves the later piece's bytes, as the
  fetch set does; and on an image the section walk serves without
  a rebase (ET_CORE, an unknown `e_type`), a read-only section that shares
  its address with an earlier writable-and-executable one answers ROM reads
  past that one's end, where fetch keeps only the earlier section.
- `.ordered()` on `int_ne`, `int_le`, `int_sle` and `float_ne` pins the
  comparison holding the pair written, not the `Xor` the lowering adds over
  it, under every wrapper; on `float_le` and `float_is_nan` it is a no-op (it
  pinned their `Or` / `Xor`). Python's `.ordered()` already did this.
- `Lifter::build_ir_with`, and so `Strider::analyze`, refuse a default
  calling convention that fails `BuiltCallingConvention::validate` (a
  struct literal; every built one is validated already). Per-address
  overrides keep the stack-geometry check alone. Convention errors say
  "stack pointer" where they said `stack_vn`.
- `instantiate` refuses a template `IntConst` built from a `NodeKind`, or
  returned by a `TemplateKind::Fn` closure: its `ConstId` belongs to another
  function's interner, or another width, so it named whatever constant held
  that id in the rewritten one.
- A raw `.input(usize::MAX, p)` (`MatcherBuilder::input` and the builders'
  `input`) names slot `usize::MAX`, so it matches nothing; it meant
  `any_input`.
- A `PostOptimizer` that changes the graph, under `OptimizerPipeline::run`
  or `run_post`, drops the memory classes, which the stamp after the
  post-passes re-derives; one that changes nothing keeps them. Every
  post-pass still drops the frame-escape verdict. `run_one` drops the
  classes after a changing pass too.
- `Function::to_text` and `to_text_named` print a
  `frame_address_in_call_register` line after `default_cc` when a call's
  clobbered register held a stack address (the fact that bars forwarding a
  load across a call); a function without it prints as before.
- `Function::to_text` and `to_text_named` print an `assumptions` line after
  `default_cc`, and before `frame_address_in_call_register`, naming the
  claims in force that are on, in declaration order, with
  `noalias_allocators=[..]` as sorted hex; a function claiming nothing prints
  as before.

### Fixed

- `first_of` keeps, at each value it is offered at each read, only the matches
  through the first listed arm that yields any match. It kept the first arm to
  match in search order, so a sibling tried around it (another `one_of`'s arm
  order, or the slot the `first_of` sat in) could report a later arm beside an
  earlier one, or instead of it where a query stops at its first match. Two
  outputs of one node, such as a `CallOther`'s result and a clobber, are two
  values: an arm matching at one no longer drops the other arm's match at the
  other. A match reaching one `first_of` more than once (`float_is_nan` and
  `float_le` read their operands twice) settles each read on its own, in the
  order the pattern reads them: of the matches still kept that make that read,
  only those through the first arm any of them took there stay. Several
  `first_of`s in one match settle in the order they are written (in Python, in
  pattern order: operands left to right, a `one_of`'s arms as listed, a
  builder's own slots in its fixed order, then its `any_input`s, then its
  `input`s, whatever order the setters were called in, a repeated setter (`arg`,
  `input`, ...) keeping its call order), an inner one before the one holding it,
  each among the matches the earlier ones kept. `Pattern::root` refuses a
  pattern unfolding to 2^64 or more reads through every arm of its alternations.
  A query holding a `first_of` runs every configuration before it answers,
  `match_at` included, unless it wants one match (`match_at`, or a pattern
  capturing nothing) and finds one through the first arm of every `first_of`,
  walking through no cast, where it stops.
- `ignore_casts` (Python) and `Pattern::ignore_casts_mask` (Rust) keep, per
  operand under a masked cast, the matches at the outermost cast level any match
  the `first_of`s kept reached. Several such operands settle in the order they
  are written, an inner one before the one holding it, each among the matches
  the earlier ones kept, as several `first_of`s do, so a guard coupling two of
  them answers by which is written first. The walk-through engaged when the
  level above had produced no match in the search so far, so a capture a sibling
  binds in some configurations and not others moved the answer:
  `int_add(var(x), s)` and `int_add(s, var(x))`, `s` binding `x` in only some
  configurations, could answer differently, as could a sibling `one_of`'s arm
  order. A query under a mask runs every configuration before it answers, on
  the terms a `first_of` does, so it costs more than the same query without
  one.
- Python: `repr()` of a `CallingConvention` from `.preserves_all()` or
  `.preserves_regs()` names the variant
  (`CallingConvention.x86_64_systemv().preserves_all()`), where it read as the
  plain preset, a convention that clobbers registers.
- Python: `repr()` of a `CastMask` is the classmethods that build it
  (`CastMask.zero_extend() | CastMask.sign_extend()`, `CastMask.none()`),
  which evaluates back to it, where it read `CastMask(0b00000011)`, a
  constructor that does not exist.
- Python: `repr()` of a `BoundCapture` whose function was compacted or
  rewritten reads `BoundCapture(stale)`, where it raised `StriderError`.
- Python: `repr()` of a `VnSpace` is its class constant (`VnSpace.RAM`), which
  evaluates back to it, where it read `VnSpace.ram`, an attribute that does
  not exist. A space with no constant reprs `<VnSpace OTHER 'c'>`, `c` being
  the shortcut its `Vn`s print with.
- Python: a `StriderError` raised on a small thread (a `threading.stack_size`
  of 32 or 40 KiB, or 64 KiB in a debug build), such as a refusal or a pattern
  nested past the thread's stack, reaches the caller, where it crashed the
  process: building it resolved a Rust backtrace on the spent stack.
  `.backtrace` is resolved when first read. `STRIDER_BACKTRACE=1` still
  resolves it where the error is raised, to fold it into the message, so a
  small thread can still crash then.
- Python: a `per_address_ccs` convention that does not resolve against the
  target's registers raises a one-line message naming the address and the
  cause, its frames on `.backtrace`. The message carried the whole Rust
  backtrace.
- `int_const_any_width` with a negative value matches an `I256` or `I512`
  constant holding it sign-extended, or an `I512` holding that `I256` value
  zero-extended, as it does below 128 bits. It matched no negative value past
  128 bits. At those widths it no longer matches a value sign-extended to 128
  bits and then zero-extended, which is no single widening:
  `int_const_any_width(163)` matched the `I256` constant `2^128 - 93`.
- Rust: a template whose width group mixes types anchors that group from the
  graph, operands first in slot order, not from build order, so where a
  capture was built no longer changes which type the group takes or the
  refusal it reports.
- Python: on a ppc64 ELFv2 image where two symbols share one local entry,
  `ElfLifter.analyze` takes a call there under the `per_address_ccs` entry of
  the lower symbol address. It took one of the two by hash order, which
  differed from one `LifterOptions` to the next.
- Rust: `instantiate` refuses a template node declaring two outputs at one
  slot, as it refuses two inputs at one slot. The later output silently
  replaced the earlier one.
- Rust: `instantiate` refuses a template capture the LHS left unbound before
  it builds any node. The nodes built ahead of the capture stayed in the
  function, dead.
- Rust: `TemplateBuilder::finish` on a graph the raw verbs wired into a cycle
  refuses the template through `Template::root`, as `MatcherBuilder::finish`
  refuses a cyclic pattern. It panicked.
- Rust: a capture one `*_const_with!` closure reads as an `Option` no longer
  relaxes the rewrite coverage check for every OTHER use of it in the same
  RHS. A value vertex (`var(c)`), an output type inheriting the binding's
  width (`capture_typed(c, ..)`) and a second closure's non-`Option` read
  have no `None` to fall back to, so an LHS alternation binding `c` on only
  some arms is refused at construction again. It was accepted, and the arm
  that bound nothing then failed `instantiate` mid-rewrite, which is not a
  skip: it aborted the pipeline for the whole function, on whichever binary
  first reached that arm.
- ARM: a direct edge to an address the build recorded no `inst_next` for
  (every forward `b`, and every conditional whose fall-through terminates
  before the taken target) now carries the ThumbEE mode live at the branch.
  It carried only the ISA-mode bit, so `TEEMode`, which the sla declares
  flowing, degraded to the entry default AND was written over the live paint
  at the target: after an `enterx`, a forward branch decoded its target as
  plain Thumb, inventing a `Store` for `stmia r2!,{r0}` where ThumbEE has a
  zero-op `hb`, with nothing reported in `Cfg.incomplete`. The mirror case
  dropped a real `Store`. The IT-block state keeps degrading, as a `noflow`
  commit covers one address.
- ARM: every arm of a resolved indirect branch (a jump table, a `mov pc, rX`,
  whether the caller seeded it or `analyze`'s own resolver did) now carries
  the ThumbEE mode live at the dispatch, as a direct edge does. It took the
  entry's defaults plus the ISA-mode bit alone, so a dispatch inside a
  ThumbEE region handed its arms `TEEMode=0`, which was then written over the
  live paint: the arm decoded as plain Thumb, inventing a `Store` for
  `stmia r2!,{r0}` where ThumbEE has a zero-op `hb`, with nothing reported in
  `Cfg.incomplete`. The IT-block state keeps degrading here too.
- Rust: `strider_target::Endianness::slice_shift_bits` returns the `None` its
  doc promises for a varnode outside the container on the little-endian arm
  too, where it compared neither the sizes nor the address space and answered
  `Some` for an overhanging or foreign-space slice. No analysis path reached
  it with such a pair; the two guards reading it were dead on every
  little-endian preset.
- A `per_address_ccs` override whose stack pointer is not the analysed
  function's is refused (`StriderError` from `Lifter.analyze`, `Err` from
  `strider_lift::Lifter::build_ir_with`), where the lift accepted it and
  produced wrong IR: the clobber split and the def-site mirror honoured the
  override's register while the post-call adjust was applied to the
  function's, so the function's stack pointer became a call clobber and its
  writeback replaced the adjust with the opaque `Call` output. Every stack
  offset past such a call was lost. An override that keeps the function's
  stack pointer, which is every one a real ABI needs, is unaffected.
- Python: AArch64 `fcvt{n,a,m,p,z}{s,u}` round to nearest even, to nearest
  away from zero, down, up or toward zero as the mnemonic says, where every
  one truncated, and saturate to the destination's range, a NaN giving 0;
  the unsigned forms converted as signed. glibc's `lround(2.5)` (`fcvtas`)
  read as 2. The scalar and general `#fbits` forms scale exactly and
  saturate too. `fjcvtzs` wraps modulo 2^32 and sets Z when exact, clearing
  N, C and V, which it left stale.
- Python: ARM `vcvt` to a 32-bit integer saturates, a NaN giving 0, and
  `vcvtr` rounds by FPSCR.RMode, taken as 0 under
  `fp_rounding_mode_is_nearest`, where it was an opaque op of the wrong bits.
- Python: AArch64 `fmov` between a general register and an h register
  moves the 16 bits, where it converted the value between half and single
  or double precision.
- Python: AArch64 `fcvt` to a narrower float, `fcvtn`, `scvtf` (general and FP
  register) and `ucvtf` (general register), ARM `vcvt.f32.f64`,
  `vcvt.f32.s32`, `vcvt.f32.u32`, `vcvtb` and `vcvtt`, MIPS `cvt.s.d`,
  `cvt.s.w`, `cvt.s.l` and `cvt.d.l`, and x87 `fst` and `fstp` to a single or
  a double round by the dynamic mode, taken as nearest even under
  `fp_rounding_mode_is_nearest`, where they rounded to nearest whatever it
  was. AArch64 `bfcvt`, `bfcvtn` and `bfcvtn2` convert to BFloat16, where they
  converted to IEEE half; `fcvtxn` rounds to odd; the general `scvtf` and
  `ucvtf` `#fbits` forms scale by an exact power of two, where the half ones
  rounded the integer to half first and divided by infinity past `#15`, and
  `#64` divided by one.
- Python: ARM `vrintr` and `vrintx` hand their op FPSCR.RMode, bits 22-23,
  where they passed bits 21-22, so two of them across a `vmsr` switching
  between nearest and toward -inf deduplicated.
- Python: x86 `fist`, `fistp`, `fisttp` and every SSE and VEX conversion to
  an integer give the integer indefinite for a NaN, an infinity or a result
  past the destination, where they took p-code's undefined `trunc` answer.
- Python: MIPS float-to-integer conversions saturate to the legacy default
  (the signed maximum for a NaN, an infinity or a value out of range),
  `round` breaks a tie to even, and `cvt.w` and `cvt.l` round by FCSR.RM,
  taken as 0 under `fp_rounding_mode_is_nearest`, where they took any mode
  but 0 as toward -inf and broke a tie away from zero. `trunc.l.s` on a
  32-bit FPU writes the whole doubleword.
- Python: PowerPC `fmadd`, `fmsub`, `fnmadd`, `fnmsub` and their single
  and record forms round the exact A*C +/- B once, where they rounded the
  product and the sum apart (the single forms three times), so gcc's
  default `a*b+c` could read one bit off and `-2^31 * DBL_MAX + inf` read
  NaN; `fmsub.` and `fnmsub.` rounded to single. The VSX `xs*m*sp` and
  `xs*m*qp` ops read their target, the addend or multiplicand.
- Python: PowerPC `fmadd` and its kin give the ISA's NaN: FRA's, FRB's or
  FRC's, the first that is one, quieted with its sign (a single's fraction
  cut to 23 bits), or the default QNaN, where `fmsub` negated a NaN B and
  the `n` forms negated a NaN result. `mtfsf` and `mtfsfi` raise FX when
  they set an exception bit outside field 0 that was clear, the usual rule
  the ISA's programming note keeps; only field 0 takes FX from the image.
- Python: PowerPC FP record forms set CR1 bit 3 to OX, where they wrote it
  into CR3's EQ bit, and `mtfsb1.` sets CR1. FX rises only when an
  exception bit is newly set, where one left set re-raised it after
  `mtfsb0` cleared it. `mtfsf`, `mtfsb0`, `mtfsb1` and `mcrfs` keep FEX and
  VX the summaries, `mtfsb1` of a clear exception bit raises FX, `mcrfs`
  of field 1 clears VXSNAN, and `fcmpu` and `fcmpo` set FPCC's unordered
  bit.
- Python: ppc64 `lxsd` loads doubleword 0 and zeroes doubleword 1, where it
  loaded doubleword 1; little-endian `stxvw4x` stores word 0 first, where it
  reversed the words; `fmrgew` and `fmrgow` merge the high and the low
  words, FRA's first, where they took the other words in the other order.
- Python: PowerPC `stfs` and its forms store the ISA's bits of fS,
  truncating, where they rounded to nearest and an exponent past a single's
  gave infinity, and `lfs` and its forms keep a signalling NaN signalling,
  so a float copied through an FPR keeps its bits. `xscvdpspn` keeps
  doubleword 1, which the ISA leaves undefined.
- Python: PowerPC `fadd`, `fsub`, `fmul`, `fdiv`, `fsqrt`, `fmadd` and its
  kin, `fre`, `fres`, `frsqrte`, `frsqrtes`, `frsp`, the conversions to and
  from an integer, `xsadddp` and their single and record forms set the
  FPSCR status they may change. FR, FI, OX and UX each come from an opaque
  `float*` op (classed pure), XX from FI, and VXISI, VXIDI, VXIMZ, VXSQRT
  and ZX from the operands; they kept those bits as the last `mtfsf`,
  `mtfsfi`, `mtfsb0` or `mtfsb1` left them, so a later `mffs` or `mcrfs`
  folded to a stale value. `fdiv` sets ZX for a finite nonzero dividend
  alone, where 0/0 set it; `fcfid` sets FPRF; `frin`, `friz`, `frip` and
  `frim` clear FR and FI. FPRF is the result's class in the instruction's
  precision, C included, which no op wrote, and FU for an infinity, where
  it was 0. VXSNAN is a signalling NaN's alone, where a quiet one set it;
  the estimates set no XX; the conversions to an integer leave FPRF
  unknown (the new opaque `fpscrUndefined`, classed pure), where they kept
  it; `fcmpu` and `fcmpo` raise VXSNAN, and `fcmpo` VXVC, where they set no
  exception bit.
- Python: x87 `fprem` and `fprem1` lift as the opaque `x87Prem` and
  `x87Prem1`, each with a status op that sets C0-C3. They computed about 0
  and left C0-C3 alone, so glibc's `fmodl` loop tested a stale C2.
- Python: x87 `fxam` classifies from the encoding: a NaN read as an
  infinity, an infinity or a denormal as normal, and -0.0 as positive. It
  and `fcom` keep the status word's other bits, where they cleared them.
- Python: `lock hlt` (F0 F4) is reported undecodable, as LOCK HLT faults,
  where it lifted as `hlt`.
- Python: each optimizer pass's `__doc__` is its stub docstring. They had
  drifted: `help(LoadForward)` said every call and control merge blocks it,
  and both texts said a `MemPhi` does, where one whose arms reach the same
  definition does not.
- Python: `ElfLifter.analyze` no longer raises when an attached symbol file
  changed after the tables built from it, which is all it reads of one. A
  change to the image itself still raises, from the read that would see it.
- Python: an empty `CfgOptions(data_ranges=...)` range and a `BufferReader`
  region past the end of the address space raise `ValueError`, as every
  other range-checked constructor argument does. They raised `StriderError`.
- Python: `Lifter.pcode_at` reads its `entry` as `analyze` and `build_cfg`
  do: an int that does not fit an address, or a symbol name, raises
  `StriderError` naming the cause, where it raised a bare `OverflowError` or
  `TypeError`.
- Python: `add_elf` refuses an image whose architecture, class or byte
  order, or on ARM whose `EF_ARM_BE8`, differs from the first image's. It
  merged one silently, and the handle's one arch decoded it as garbage: a
  BE8 image beside a BE32 one decoded byte-swapped.
- Python: `Function.to_text` prints the frame reach a call register held,
  `frame_address_in_call_register reach=<offset|unbounded>`, which the
  incoming-argument escape verdict reads. Two functions that differ only in
  the reach printed the same text.
- Python: a `strider.template.float_const` with a set bit above the width it
  is evaluated at refuses the rewrite, as Rust `float_const` and
  `float_const_with_fn` do. The bits were masked to the width, so an f64's
  bits at an F32 operand (`1.0` as `0x3ff0000000000000`) built `0.0`.
- Python: `Function.rewrite` / `rewrite_all`, and Rust `apply_rules_count`,
  count a site once. A match whose only consumer an earlier fire of the same
  walk had replaced was still rewritten and counted, and its fingerprints
  absorbed into the surviving value.
- Python: `Function.rewrite` / `rewrite_all`, and Rust `rewrite_rule`, refuse
  a match through a `Phi`, `MemPhi` or `Region` whose replacement reads the
  value it replaces with no phi between them, or is not available wherever
  that value is. `phi(a, b) -> b` on a loop left a non-phi data cycle, and on
  a merge a value defined on one arm only, both reported as a successful
  rewrite.
- Python: under `relocated_got_is_rom=True` a MIPS global GOT entry is
  served only when the `.dynsym` header is `DT_SYMTAB`'s table; a header
  moved onto a decoy symbol served an import's entry as its lazy stub.
- Python: a dynamic image's relocation tables are each walked as they lie,
  and what the loaders could read differently is a hole: every place of
  two tables that overlap off each other's entry boundaries, a place two
  entries relocate or one `REL` entry relocates twice, and a place in the
  dynamic array. A relocation inside a relocation table, which rewrites it,
  makes the table unreadable. When a table is unreadable, a relocating load
  serves no writable mapping, `read` included, where it served them
  unrelocated. An ELF32 Android stream reads its fields at 32 bits, as
  bionic does, and its entries count against the budget at their plain
  size. A `.dynsym` is `DT_SYMTAB`'s only when its header holds the whole
  table, as far as `DT_HASH` / `DT_GNU_HASH` count it.
- Python: a ppc64 ELFv1 relocatable's `.opd` relocations are walked once per
  distinct table and within the relocation entry budget. Repeated headers
  over one `.rela.opd` cost their count times its length, unbounded, in
  `load_elf` and in symbol lookup alike; past the budget no descriptor is
  followed.
- Python: a dynamic relocation field no one mapping holds whole, over two
  adjacent read-only mappings or past the end of the one holding its start,
  is a hole. It was neither patched nor holed, so its mapped bytes served
  the file's.
- Python: the `load_elf` and `add_elf` docs say that `apply_relocations=False`
  serves every relocation field still mapped, a `.rodata` table's and a
  TEXTREL image's code included, as its file bytes.
- Python: under `relocated_got_is_rom=True` the two GOT header words the
  loader writes for lazy binding are unmapped: ARM's at `DT_PLTGOT + 4` and
  `+ 8`, and secure-PLT PPC32's after `DT_PPC_GOT`. They served the file's
  zeros, so PLT0's `ldr pc, [lr, #8]!` folded to a jump to address 0.
- Python: under `relocated_got_is_rom=True` a 64-bit TLS slot (a module id,
  module offset or TP offset) is unmapped over all 8 of its bytes, and a
  TLS descriptor over both its words, on x86-64, AArch64, ppc64 and MIPS64,
  as is an i386 or ARM `TLS_DESC` slot over its 8. Only the first 4 bytes
  were, so a 4-byte read of the upper half of a negative TP offset folded
  to 0.
- Python: a linked image with a `PT_DYNAMIC` is relocated from the tables
  its dynamic tags place, read as `ld.so` and bionic read them: the last
  `PT_DYNAMIC`, at its `p_vaddr`, and the last of each tag, placing
  `DT_RELA`, `DT_REL`, `DT_JMPREL`, `DT_CREL` and Android packed (`APS2`)
  tables. No section header decides it any more: an image with its headers
  stripped (`llvm-objcopy --strip-sections`), retyped or decoyed was
  relocated from them, or not at all, so a TEXTREL `mov loc,%eax` decoded
  as a load from address 0, and an Android packed library's pointers
  served the file's 0 under `relocated_got_is_rom=True`. A symbol resolves
  only through a `.dynsym` header at `DT_SYMTAB`. A table the tags place
  that cannot be read or decoded makes `load_elf` and `add_elf` raise
  `StriderError` when the image relocates code, and otherwise leaves
  `.got` and `.data.rel.ro` unserved under `relocated_got_is_rom=True`.
- Python: a MIPS32 `R_MIPS_COPY` in a relocation table not linked to
  `.dynsym` is a hole to the end of its mapping, as on the other arches;
  it was holed over one word.
- Python: under `relocated_got_is_rom=True` every byte of an `R_*_COPY`
  object is unmapped. Only its first 4 bytes were, and the rest served the
  file's zeros, so a gcc PIE's `return tbl[3]` against a library's const
  array folded to 0.
- Python: a jump table indexed by a byte read through a pointer is no
  longer enumerated over the whole byte when a test dominating the dispatch
  bounds that byte in a shape the range analysis does not read (`test $0xfc;
  jne`, `shr; jne`, a sign test), or guards a reload of the same field. The
  cells past the table were seated as arms with nothing reported; the site
  is now deferred and reported as `unresolved_branch`.
- Python: an ARM or Thumb `blx #imm`, or a MIPS `jalx`, to a callee below
  the caller no longer switches the ISA mode of the caller's own successors.
  The call commits the mode at its target, painting forward over the caller,
  and the edges the caller's region enqueued read it there, so on a fresh
  engine the ARM `nop` after the `blx` in clang Thumb kernels' `__bad_stack`
  decoded as a phantom Thumb call, and a MIPS `jalx` failed to build.
- Python: PowerPC `fctid` and `fctiw` round by FPSCR.RN, where they
  truncated, and `fctidu` and `fctiwu` break a tie to even, where they broke
  it away from zero (all four under `fp_rounding_mode_is_nearest`, which takes
  RN as 0). Every float-to-integer form, the `z` ones too, saturates a value
  out of range and gives the ISA's result for a NaN (the most negative value,
  0 for the unsigned forms), where it gave the x86 indefinite.
- Python: PowerPC `mtfsfi` writes its immediate U to the field, where it
  wrote U shifted left one bit, into the next field.
- Python: PowerPC `fcfidu` and `fcfidus` convert the doubleword as
  unsigned. They converted it as signed, so `(double)(1ull << 63)` gave
  -9.22e18. `frsp`, `fcfid`, `fcfids`, `fcfidu`, `fcfidus` and `xscvdpspn`
  round by FPSCR.RN without `fp_rounding_mode_is_nearest`; they always
  rounded to nearest. The signed `fcti*` forms set VXCVI when the
  conversion is invalid, and `mtfsfi` keeps FEX and VX the summaries of the
  bits they cover for every field, where field 0 took them from U and the
  others left them stale. FEX is the OR of the enabled exceptions, where two
  cancelled, and FX rises with VX.
- Python: PowerPC `friz` (gcc's `trunc()`) rounds toward zero to a double.
  It wrote the integer's bits, so `friz` of 2.5 read as about 1e-323.
- Python: ppc64 `xscvdpspn` reads doubleword 0 of its source and writes the
  single to words 0 and 1, so gcc's `xscvdpspn; mfvsrwz` float-to-bits pun
  gives the IEEE single's bits; it read doubleword 1 and wrote word 3, and the
  pun returned stale bits. `lxsdx` and `xsadddp` use doubleword 0, which the
  FPR of the same number overlays, where they used doubleword 1.
- Python: ppc64 `xsadddp` sets the FPSCR bits `fadd` sets (FPRF, VXSNAN and
  the summaries). It set none, so an `mffs` or `mcrfs` after it read the bits
  from before it, and a branch on them folded. It shares `fadd`'s two flaws:
  VXSNAN is set for a quiet NaN too, and XX ORs in the FI no p-code op sets.
  No scalar float op the sla spells in p-code sets FR, FI, OX, UX, VXISI,
  VXIDI, VXIMZ or VXSQRT: those keep the last explicit write's value, so
  `mtfsb0` FI, `fadd`, `mffs` reads FI as 0.
- Python: ppc64 `xsdivqp` lifts to its own `call_other`, `vsx300_xsdivqp`,
  appended after the existing user ops, so no other id moved. It shared
  `xscvudqp`'s `vsx300_47`, so a query by name could not tell the two apart.
- Python: the ppc64 presets lift e500 `mfpmr` and `mtpmr`, each read of a
  performance-monitor register its own value. A function using one failed
  to lift on an unclassified `call_other`.
- Python: the `ppc32be` and `ppc32le` presets decode `isel`, which clang
  emits for ppc32 (`iseleq r3,0,r3`). It was undecodable, so a function
  reaching one failed its build or lost the jump-table arm that held it.
- Python: a PowerPC jump table whose guard or index is an `rlwinm` resolves.
  A guard on a rotate held below `2^r` (`rotlwi r4,r3,29; cmplwi r4,7`)
  bounds the rotated register to its multiples of `2^(32 - r)`, and a rotate
  under a mask that drops one of its halves (`srwi`, `extrwi`, an index
  scaled into place) reads as the bit field it extracts, so a guard on the
  field bounds the index. Such sites were unresolved.
- Python: a jump table indexed by the low byte of a wider load, as x86-64's
  `mov 0x10(%rdi),%eax; movzbl %al,%eax` lifts it, is read the way the
  byte itself is: through a pointer it resolves over the byte's 256 values,
  and as a cell of a table it still defers. The width count stopped at the
  truncate, which the walk naming the cell already stepped through.
- Python: `relocated_got_is_rom` serves PPC32's `.got2`, the per-object GOT
  a `-fPIC` function loads its jump-table base from after
  `bcl 20,31,$+4; mflr r30`, as it serves `.got`. Such a table was never
  resolved.
- Python: a jump-table guard on one zero-extend of a value bounds a phi arm
  that is another zero-extend of it, as in a `jhash` tail `switch (len)`
  whose `len` is a loaded byte; the guard bounded only the very value it
  compared.
- Python: a jump table indexed by a byte (or a scaled byte) read through a
  pointer resolves over the byte's width, as `psmouse_process_byte`'s
  `movzbl 0xcb(%rbx)` / `shr $6` does. A byte whose address adds an index
  that a guard or its own width bounds is a cell of a table, served by ROM
  or not, and still defers. It was deferred, unless a 32-bit register read
  put a truncate between the extends.
- Python: `ConstantFold` turns a truncate of an extend to a width other
  than the source's into one cast: a truncate of the source below its
  width, the same extend of it above, so `zext64(trunc32(zext64(x8)))` is
  `zext64(x8)`. Only the truncate back to the source's own width folded.
- Python: `ConstantFold` folds a bitcast (`int_bits_to_float`,
  `float_bits_to_int`) of a constant up to 64 bits to a constant of the same
  bits, as the builder does. Only a bitcast built over a constant folded, so
  one whose operand a fold made constant stayed a bitcast, hiding its value
  from patterns and from the folds after it.
- `ConstantFold`, `LoadReadOnly` and the jump-table evaluator fold a
  constant shift whose count is narrower or wider than the shift: they asked
  the count for the output's width, which p-code never asks of it.
- An index range joined with an empty one is the other range, its lower
  bound and stride kept; the join widened the lower bound to 1 and dropped
  the stride.
- Python: a ppc64 ELFv1 function's `Symbol.size` is its `st_size`, which
  GCC and Clang give the code, unless a dot symbol names the code or the
  size is a descriptor's (16 or 24), as in a kernel. It was `None` for every
  function whose descriptor was followed, so `analyze(name)` on an image
  with no dot symbols is now bounded by that size, as on any other arch.
- Python: a relocation section header repeating another's table (the same
  type, file offset and size, and on an object file the same target) is
  walked once. In a linked image where the repeats disagree on whether the
  entries index `.dynsym`, every entry naming a symbol is a hole. An image
  whose relocation section headers name more than twice the entries the
  file holds at its smallest entry (an `SHT_REL` one, or a byte with an
  `SHT_CREL` table), counting no table that runs past the file, is refused
  with an error: N headers over slices of one table of M entries recorded
  up to N x M patches.
- Python: in a linked image with a `.dynsym`, an allocated relocation
  section (`SHT_REL`, `SHT_RELA` or `SHT_CREL`) linked to `.symtab` or to
  nothing is applied: an entry naming no symbol as the dynamic table's are,
  one naming a symbol (and on MIPS every entry) as a hole. Neither walk
  read it, so its fields served the file's bytes. Such a hole spans the
  word unless the type names its field, in every mapping the field
  reaches; a MIPS `R_MIPS_NONE` writes nothing.
- Python: under `relocated_got_is_rom`, a `.got` or `.data.rel.ro` whose
  `sh_size` runs past the next allocated section, or past the PT_LOAD
  holding it, is served only up to there. The ROM view served the writable
  bytes it ran over.
- Python: an x86-64 `R_X86_64_32` whose value does not zero-extend is a
  hole, as GNU ld refuses it; one that fit only sign-extended was patched.
- Python: the PowerPC VSX arithmetic, rounding and conversion ops
  (`xsaddqp`, `xsdivsp`, `xscvqpswz`, ...) set the FPSCR status bits they
  raise (FPRF, FR, FI, FX, OX, UX, ZX, XX, VX*); the vector conversions,
  the compares writing XT and max/min set their exception bits, and
  `xvcmpnedp.` / `xvcmpnesp.` CR6 too. They were classed as pure, so an
  `mcrfs` or `mffs` after one read the FPSCR as it was before, and a branch
  on it folded.
- Python: `PhiCollapse` folds `phi & C` to `v` when every arm masked by
  `C` is `v`, an arm that reads the mask back through the loop agreeing. An
  ARM interrupt-mask restore after a loop that saves again on its back edge
  (`mrs r5, cpsr; cpsid i` before the loop and on the back edge, then
  `msr cpsr_c, r5`, Linux's `dump_stack_lvl`) now keeps the mode without
  `msr_register_keeps_mode`, so its return resolves. It was reported as an
  unresolved branch.
- Python: x86 `hlt` falls through to the next instruction, as it resumes
  once an interrupt is serviced, and clobbers memory (the new `halt`
  `call_other`). It branched to itself, so `sti; hlt; cli; ret` (Linux's
  `default_idle`) never returned and nothing was reported. A libc `_start`
  now runs from its `hlt` into the bytes after it. `hlt` is privileged; to
  treat it as no-return in userland code pass
  `call_other_abis={"halt": CallOtherAbi.no_return()}`. `halt` was
  appended after the existing x86 user ops, so no id changed.
- Python: `load_elf` and `add_elf` raise `ValueError` on a microMIPS image
  (`EF_MIPS_ARCH_ASE_MICROMIPS`), which no preset decodes. It loaded as
  plain MIPS, and a function entered at its ISA-bit address decoded as
  MIPS16e into a complete, wrong function. `load_elf` with both `arch=` and
  `cc=` still loads one, to lift its MIPS32 code.
- Python: a seeded jump-table arm whose code falls through into
  `data_ranges` or unmapped bytes is dropped (Rust:
  `Cfg::undecodable_seeded_targets` names it), as one falling past the
  function's end is. It was kept with a stub, so a bad arm survived with
  its garbage and the resolver never froze its site. An arm whose call
  precedes those bytes is kept, the call not returning.
- Python: in a linked image with no `.dynsym`, a dynamic relocation naming a
  symbol is not computed. A read-only word is a hole in every view, and a
  writable one serves its file bytes. It resolved through `.symtab`, whose
  index means something else, and served an unrelated symbol's address
  (four `R_AARCH64_ABS64` words of an arm64 5.14 kernel).
- Python: `Function.rewrite` / `rewrite_all` (and a Rust `rewrite_rule`)
  raise `StriderError` where the match holds no node an instruction produced
  (such as a bare `initial_var` or `phi` root) and the replacement builds
  nodes. It built nodes with no instruction address, which `validate`
  rejected, and the next `optimize` failed. A replacement that declines is
  still no change.
- A signed compare against an immediate (`cmp edi, 5; jl`, `cmp w0, #5;
  b.gt`, `cmn r0, #5; bge`, `cmp r0, #0; ble`, and a loop's `add w1, w1,
  #1; cmp w1, #15; b.le`) folds to one `Sless`, as a register compare does,
  on x86, ARM, Thumb and AArch64. Its flag tree survived, so a `Sless`
  pattern query, from Rust or Python, missed the branch. `Sborrow(x, 0)` and
  `Scarry(x, 0)` fold to false.
- PowerPC `slbmfee`, `slbmfev` and `slbfee.` write RT, `xsiexpdp` writes XT
  and `vminud` writes vrD; each left the old value in place, so `slbmfev r5,
  r6; mr r3, r5` returned the incoming r5. `xscmpexpdp`, `xscmpexpqp`,
  `xscmpoqp`, `xscmpuqp`, `xststdcdp`, `xststdcqp` and `xststdcsp` write CR
  field BF, and a branch on it no longer folds on the field's earlier value.
  They also set FPSCR.FPCC, and `xscmpoqp` / `xscmpuqp` the invalid-operation
  bits, which an `mcrfs` or `mffs` after one read unchanged.
  `paste.` sets CR0 from the paste: it forced EQ, so the failure path of a
  `paste.; beq` was deleted.
- ARM `msr` writing the CPSR control field (`cpsr_c`, `cpsr_fsxc`, ...),
  A32 and Thumb-2, re-banks registers as `cps` does when it writes a mode
  other than the current one: it switched mode silently, so `mov r8, #5`
  under FIQ read back 5 under SVC. An interrupt-mask restore, which writes
  the current mode back, re-banks nothing, and the mode survives a call
  between the two (the ARM conventions keep the new `cpsr_mode` register
  callee-saved). `mrs` reads CPSR.M and the A, I and F masks that `cps` and
  `msr` set, which it read as zero, so `irqs_disabled_flags(flags)` no
  longer folds to "enabled". Execution at PL1 or higher is assumed: a
  User-mode write to M, A, I or F, which the hardware ignores, is recorded.
  A mode switch, `cps` included, also re-banks the SPSR. Under the default
  `msr_register_keeps_mode` claim the register form is taken to write the
  mode in force and re-banks nothing; `AssumptionOptions::none()` keeps the
  re-bank.
- ARM `msr` sets N, Z, C, V and Q only through its flags field, from its
  source: the A32 register form overwrote them for every mask, the
  immediate form never set them, and Thumb `msr APSR_nzcvq, rN` read r0
  and ORed the bits in. Thumb-2 `cpsie f, #mode` disabled FIQ.
- ARM `vld3`, `vld4`, `vst3` and `vst4` (multiple structures) transferred
  from one register before their first, `{d4-d6}` writing d3-d5.
- BE8 (`arm_be_kernel`) and BE32 (`arm_be`) `vld1` and `vst1` (multiple single
  elements) of `.8`, `.16` and `.32` move one element per access: one
  8-byte access put element 0 at the high end. `arm_be_kernel` now decodes
  with its own sla, `ARM8_be8`, whose registers are `arm`'s.
- Python: a custom `CallingConvention` or `CallOtherAbi` built on `arm`,
  `arm_thumb` or `arm_be_kernel` serves a `Lifter` of any of the three; the
  check compared slas, which `arm_be_kernel` no longer shares.
- x86 SSE and VEX float-to-integer conversions that round (`cvtsd2si`,
  `cvtss2si` and their VEX forms, `cvtpd2dq`, `cvtps2dq`, `cvtpd2pi`,
  `cvtps2pi`) break a tie to even, the default MXCSR mode: 2.5 converted to
  3. `vcvtsd2si r64` and `cvtps2pi` returned the rounded value's float bits,
  and `cvtpd2dq`, `cvtps2dq` and `cvtpd2pi` truncated. They assume RC's
  default under `fp_rounding_mode_is_nearest`, and read RC without it.
- x87 `fist`, `fistp` and `frndint` round by the control word's RC (nearest
  even, down, up or toward zero) instead of always breaking a tie away from
  zero. gcc's i386 `(int)x`, which sets RC to truncate around a `fistp`, now
  folds to a plain truncation, where 2.7 converted to 3; with no `fldcw` the
  conversion selects on the control word the function was entered with.
- x86 `rcl` and `rcr` by a CL or imm8 count of one set OF; it was left stale.
- The x86 fixes of upstream Ghidra b16f1556d2 are in the vendored spec:
  `fcmovnbe` moved when CF was set; a one-bit `shr` cleared OF rather than
  copying the sign bit; `arpl` adjusted on CF; `daa` and `das` took CF from
  the adjusted AL and left SF, ZF and PF stale (`das` also keeps the borrow
  of its low-digit adjust, which upstream drops); MMX `psubb` skipped byte
  lane 1, `vmulps ymm` lane 7, and `vtestps ymm` read bits 160 and 224 as
  sign bits; `rdtscp` and `vmclear` did not decode outside 64-bit mode;
  `vfpclasssd`, `vfpclassss` and `vrangess` decoded without their imm8, and
  `wrssq` and `wrussq` not at all. EVEX `vmovhlps` and `vmovhpd` took the
  wrong half of SRC1; EVEX `vucomisd` and `vucomiss` wrote the register
  instead of the flags; masked `vandps ymm` and `vpsubsw zmm` dropped the
  op's result; `fdecstp` cleared C0 rather than C1; `syscall` left RCX and
  R11 unwritten in p-code and 64-bit `sysexit` returned to RCX; `maskmovdqu`
  wrote XMM1; `pcmpestri` and `pcmpestrm` did not read EAX and EDX. `fyl2x`
  and `fyl2xp1` call `log2`, which the CallOther table classes as pure.
- Python: on ppc64 ELFv2, `ElfLifter.analyze` and `ElfLifter.build_cfg` add
  the local entry of every `no_return_targets` address that is a symbol's
  `address`, and `analyze` of every such `per_address_ccs` key. A `bl`
  sharing the callee's TOC targets the local entry, so a table keyed by
  symbol matched no call: a no-return call fell through into the code after
  it, and the CFG still reported itself complete.
- On `arm_be_kernel` (BE8), a multi-structure `VLD2/3/4` or `VST2/3/4` put
  each element in the wrong half of its D register: the lifter's
  REGISTER-space loads and stores were read in the big-endian data order,
  where the sla's register block is little-endian. A `Function` now carries
  the REGISTER space's byte order beside the data order, and a register-space
  load narrowed from a wider store takes it, so that load is forwarded again.
- A rewrite template's fresh constant at a shift count is built at the
  shift's width; under an `I1` root it took the root's and was masked, so
  `int_truncate(int_shr(a, 31))` shifted by 1. A count too wide for the shift
  saturates, as p-code tests the whole count. A fresh operand of a
  `Popcount`, `Lzcount` or other slot that owes its node no width is refused
  rather than built at the root's width.
- Python: `strider.template.capture_typed` over a capture or `var()` raises
  `StriderError`; in Rust, `capture_typed(c, var(x))` and `set_value_ty` on a
  capture make `instantiate` return an error. Both panicked.
- A table dispatch through a phi of constants seats only the slots those
  constants name: an index in `{0, 3, 5}` read all six slots of `0..=5`, and
  an address in `{t, t+8, t+0x20}` the two between `t+8` and `t+0x20`, which
  the jump can never take. A jump to a phi of labels (`cmov; jmp *reg`), which
  was unresolved, now seats each label.
- A jump table indexed by a bit field no longer reads past a guard written as
  another extract of the same bits (mips64 `srl; sltiu` beside `ext`): the
  guard bounds the index, where the field's whole range seated the next
  table's arms. A guard on wider bits bounds a field inside them
  (`w < 20` puts `(w >> 4) & 7` at most 1; a guard's low end only at the top
  of its bits, where the field cannot wrap), and a field bound beside a guard
  on overlapping bits that neither encloses nor equals it defers the site. A
  32-bit compare now also bounds a `clrldi` index on ppc64.
- A seeded jump-table arm dropped because it is unmapped is named in
  `cfg.incomplete` as `unmapped_target`, one whose code falls through past
  `function_max_size` as `past_function_end`, and one whose decode fails for
  any other reason but bytes Sleigh rejects as `undecodable_target`; only its
  site was named before.
- Python: a `known_targets` seed of one target outside the function, an
  indirect tail call, is seated as that tail call and reported
  `"unverified_seed"`; it was read as a one-arm jump table, which defers, so
  the site stayed in `unresolved`.
- Seeding a one-arm table at a dispatch the classifier reads as a constant
  past the function seats that tail call, as an unseeded run does, and so
  does a one-arm table the loop derived itself and a later round re-derives
  as that constant (`UnverifiedSeed`); the loop stopped on the round that
  deferred the table and reported it `UnresolvedBranch`.
- A stack-held jump-table slot, a return slot and a stack-arg slot each see
  a store whose bytes wrap onto it around the address width.
- A one-arm jump-table answer stays an arm through a merge, and a merge
  answers `Single` only for a `Single`'s own target.
- `DeadBranchElimination` without `CfgDetach` strips the dead predecessors it
  leaves; such a pipeline failed validation.
- `R_PPC64_REL32` past +-2 GiB, and any word relocation that does not fit its
  field, is a hole rather than a truncated word.
- A mips64el `SHT_REL` hole names its relocation type, not the symbol index.
- A Thumb `BL` to ARM whose relocated value is -1 or -2 encodes `BLX` with
  offset 0; it overflowed, a panic in release builds.
- A seeded `analyze` no longer copies the seed table for each function with
  a dispatch site; no longer reports or freezes a seed that is no dispatch
  site of the function because its table names an address decoded in two
  ISA modes there; and keeps the caller's seed of a `BranchIndirect` whose
  sibling in the same instruction it gave up on.
- A seed listing one address twice seats one arm, and a seed's `Switch` arms
  come out sorted.
- A `LinkRegister` (Python `"return"`) seed over a p-code `RETURN` whose
  target is not provably the caller's reports it `UnverifiedSeed`; the seed
  was ignored there, and the site stayed `UnresolvedBranch`.
- A `[]` seed no longer hides a return the classifier derives at that site,
  nor reports it `UnverifiedSeed`; a `LinkRegister` seed seats its `Return`
  where a round's classifications contradict every target they name.
- A target two classifications prove in opposite ISA modes at one dispatch
  is reported however many anchors share it, and dropped unless a seed
  names it.
- Return-target verification on a 128-bit stack offset at the edge of the
  `i128` carrier reports the return unverified; it panicked.
- At `I256` / `I512`, `And(x, 2^128 - 2)` is no longer read as the
  interworking `& ~1` when recognising a return target: at those widths it
  also clears bits 128 and up.
- `ConstantFold`'s shift-factor rules (`x + (x << c)` -> `x * (2^c + 1)` and
  kin) read a constant shift count unsigned, as p-code does: a boolean count
  of 1, or a count typed wider than 128 bits, now folds.
- The lifter's p-code constant folder treats an arithmetic result written to
  a temporary of 11, 13 or 15 bytes, or past 16 bytes, as unknown; it used to
  wrap it at 128 bits or with a mask no value type has.
- A mips64el `SHT_REL` relocation is dispatched by its real type; a lone
  `R_MIPS_16` / `32` / `64` is still a hole. Its `R_MIPS_NONE` /
  `R_MIPS_JALR` markers were holes, so the null `R_MIPS_NONE` BFD puts at
  `r_offset` 0 of every PIE and `.so` hid the image's first four bytes, and
  `R_MIPS_26` / `HI16` / `LO16` / `HIGHER` / `HIGHEST` / `PC16` were never
  applied. A mips64el `SHT_CREL` entry, which was read as a transposed
  `SHT_REL` one, is applied as written; in an image that also holds an
  `SHT_CREL` table a mips64el `SHT_REL` entry is read as written too.
- A relocation whose value reads the symbol is a hole against STN_UNDEF
  (`r_sym` 0): an instruction relocation such as `R_AARCH64_CALL26` was
  computed with the symbol taken as 0.
- Python: `.when()` on a `function_arg(..)` nested as an operand runs; it
  was dropped, so the operand matched every argument its builder named.
- Python: `m[a] == m[b]` compares the two captured constants; it was always
  `False` in the abi3 wheel.
- A pattern dropped from a thread-local destructor after strider-pattern's
  drop pit was torn down aborted the process; it is dropped in place.
- Python: `f.optimize()` no longer drops the `noalias_allocators`
  `analyze` ran under, and with them every `heap_only()` match: claims
  accumulate on the function instead of being replaced.
- `CallStackArgCollect` no longer attaches a stack argument that one
  path into the call overwrites in part past its first byte, such as an i386
  `double` whose upper dword one branch zeroes; it attached the original
  store's value as if every path passed it.
- Python: each read of a volatile register is its own value, a `CallOther`
  named `read_volatile`: every AArch64 system register but `sp_el0`, as the
  pspec marks them all volatile; the PowerPC timebase, as its pspec marks
  it; and the counters and hardware-set registers the pspecs omit, the
  PowerPC decrementers, PMCs, PURR/SPURR, VTB, ATB, IC, 4xx timers and 8xx
  breakpoint counters, and MIPS CP0 `Count`, `Random`, `Cause`, the
  performance counters and Octeon `CvmCount`. Two reads were one value, so
  `t1 - t0` folded to 0 and a timing loop's exit test never changed.
- Python: `load_elf(relocated_got_is_rom=True)` leaves unmapped the MIPS
  primary GOT entries the loader writes without a relocation record: the
  lazy-resolver word, the module-pointer word when its top bit asks for one,
  and every global entry except those for symbols the image defines that
  already hold their address. An imported symbol's entry was served as the 0
  or lazy-binding stub the file holds, so `&ext_var` folded to NULL. A
  dynamic MIPS image missing one of the tags that place its GOT leaves the
  whole `.got` unmapped.
- Python: under `relocated_got_is_rom=True` an IFUNC's slot is unmapped: an
  `R_*_IRELATIVE` one, and one relocated against a defined `STT_GNU_IFUNC`
  symbol, whose `st_value` is the resolver. Both served the resolver's
  address where the loader stores what it returns, so a call through one
  called the resolver, or lifted its body into a PLT stub. An unlinked
  object's GOT slot for such a symbol stays unmapped too, and so does any
  other data word relocated against one; in an executable mapping, where it
  may be an instruction, such a relocation keeps the symbol's address, so an
  address taken in code (`lea f(%rip)`) or a data word mapped executable
  still folds to the symbol rather than to the resolver's result.
  `ElfLifter.read` of a RELA `IRELATIVE` slot serves the file's bytes rather
  than the resolver's address.
- Python: under `relocated_got_is_rom=True` an unstripped static
  executable's `IRELATIVE` slots are unmapped too. Its `.rela.plt` links to
  `.symtab`, there being no `.dynsym`, and was never walked, so a jump
  through one of those slots went to the word the file holds, busybox's PLT
  stub looping back to itself.
- Python: an i386 object's `R_386_GOT32` / `R_386_GOT32X` with no base
  register (`-fno-pic -fno-plt`'s `call *ext_fn@GOT`) names its GOT slot's
  address; it was the slot's offset from the GOT, so the call loaded its
  target from that offset taken as an address. A `lea ext_fn@GOT, %reg`,
  which GNU ld gives the offset and lld the address, is a hole.
- Python: an AArch64 object's `R_AARCH64_GOT_LD_PREL19` with a non-zero
  addend is a hole, as `ADR_GOT_PAGE` and `LD64_GOT_LO12_NC` already were;
  `ldr x0, :got:foo+16` read the slot 16 bytes past foo's, another symbol's.
- Python: the `noalias_allocators` docs state the whole claim the analysis
  rests on: a listed call's result overlaps no stack slot and no global
  either, so an allocator carving from a static or caller-supplied arena does
  not qualify. They said only that it overlaps no other allocation.
- Incoming-argument detection no longer takes the reload after `h(&a)` as
  the argument under the default `assume_incoming_args_survive_calls`: where
  a stack address that may point into the incoming arguments' memory reaches
  a callee, every call keeps those slots unknown. That memory includes the
  register arguments' homes on ARM, MIPS and PowerPC64, so `h(&s)` on a
  struct split between registers and the stack counts too, as does an
  address built by arithmetic the analysis cannot place. A pointer to a
  local leaves the claim in force.
- The docs no longer say that call-boundary claims never reach a decoded
  branch target. The jump-table walk leaves them out of its own steps, but
  it reads the IR `LoadForward` optimized under them, so with
  `escape_analysis` and `callee_preserves_stack_args` a branch target spilled
  across a call that overwrites it seats the stale target.
- Python: the `x86_linux_kernel` docs say that a variadic or `asmlinkage`
  callee takes every argument on the stack, so under that convention its
  first argument reads as `arg(3)`, and give the recipe:
  `per_address_ccs={callee: CallingConvention.x86_cdecl()}`.
- Python: with `return_slots_survive_untracked_writes` cleared, a call whose
  convention preserves memory no longer makes a return after it unverified,
  as every other memory walk already steps it.
- A decode through `ElfFileMemReader` that lands on a relocation the loader
  does not compute names the relocation type, where it said the address was
  not mapped; a Python `BufferReader` already did.
- Python: a `flag != 0` branch test folded to the flag's comparison keeps
  the addresses of the test's widened flag and operands in that comparison's
  `asm_fingerprint`, as the `Truncate` spelling of a PowerPC CR-bit test
  already did. The operands include the shared `IntConst(0)`, whose
  fingerprint is the union of every fold into it, so a comparison can gain
  addresses from outside the flag test. Fingerprints gain addresses and
  lose none.

### Added, Rust

- `strider_graph::Graph::add_node_output`, which adds an output to an
  existing node, and `Graph::value_kind_mut`.
- `IRViewer::int_const_ibig`: a constant read as two's complement at any
  declared width, where `int_const_i128` answers `None` past 128 bits.
  `strider_ir` re-exports `IBig` beside `UBig`.
- `strider_pattern::int_const_all_ones()`: the all-ones constant of whatever
  integer width it is matched at. `int_const` names one fixed value, so it
  cannot spell a width-relative all-ones.
- `MemRegionsLookupTable` implements `rsleigh::MemReader`, the one fetch
  adapter `ElfFileMemReader` and the Python `BufferReader` share.
- `AssumptionOptions::fp_rounding_mode_is_nearest`, default on, read by the
  lift through the fork's new user ops, each appended after the upstream
  ones: x86 `mxcsrRoundingControl` and `x87RoundingControl`, PowerPC and ARM
  `fpscrRoundingControl`, AArch64 `fpcrRoundingControl` and MIPS
  `fcsrRoundingControl`
  (`strider_target::call_other_abi::is_rounding_mode_control`). AArch64's
  takes no input and is FPCR, which the lift reads only with the claim off
  (`strider_target::call_other_abi::rounding_mode_register`).
- `strider_pattern::instantiate_guarded`: `instantiate` with a check run
  once every template node's kind is resolved, before the first node is
  created.
- `AssumptionOptions::msr_register_keeps_mode` (default on), the claim the
  Python field above carries, read by the lifter. The ARM sla yields a
  register-form `msr`'s mode through the user op `msrRegisterMode(new,
  current)`, which `strider_target::call_other_abi::is_msr_register_mode`
  names and the lifter lowers to `current` under the claim, else `new`.
- `strider_reader::elf::local_entry_offset`: how far past a ppc64 ELFv2
  symbol a same-TOC call enters.
- `strider_target::ByteOrder`, the data and REGISTER-space byte orders, from
  `SleighArch::byte_order` and `Function::byte_order`. `Function::new` and
  `FunctionBuilder::new` take one, or a bare `Endianness` for every space.
- `Reason::PastFunctionEnd`: where code fell through past a
  `FunctionBound::Size` end.
- `Function::to_text_named(fingerprints, &ArchNames)`: `to_text` with
  registers spelled by name.
- `IRViewer::switch_selector`: a `Switch`'s dispatch value.
- `Strider::lifter()`, the engine `analyze` lifts with.
- `strider_cfg::seated_in`: the `known_targets` entry for a site, by its exact
  key or else its instruction's p-code index 0, as the CFG seats it.
- `strider_cfg::seated_from`: what `CfgOptions::seated` reads, over any
  two tables.
- The orchestrator's `test-util` feature: `Strider::analyze_with_classifier`,
  `analyze` with a `Classifier` standing in for `classify_sites`.
- `BuiltCallingConvention::check_addr_size`.
- `ALL` on every op enum (`IntBinaryOp::ALL`, `IntCmpOp::ALL`, ...): the
  variants in declaration order.
- `Graph::NODE_RECORD_SIZE` / `VALUE_RECORD_SIZE`: the arena's bytes per node
  and per node output.
- `OptimizerPipeline` is `Clone`, and `add_boxed` / `add_post_boxed` take a
  pass already boxed.
- `Function::assumptions` / `assume` and `SideTables::assumptions`: the
  claims in force, which every pass reads, and the one verb that adds to
  them, dropping the memos a new claim can change.
- `strider_opt::refresh_memory_classes`: re-derives what `stack_only()` /
  `heap_only()` read after an edit made outside a pipeline.
- `EditFunction::absorb_node_fingerprint` (the node-keyed
  `absorb_fingerprint`) and `EditFunction::clear_arg_values_from`, so no pass
  needs `function_mut()` for an overlay write.
- `IRBuilderExt::build_all_ones(ty)`: the all-ones constant at any integer
  width.
- `strider_ir::eval`: integer evaluation at a declared width (`int_binary`,
  `int_cmp`, `int_unary`, `sign_extend`, `popcount`, `lzcount`), total at
  every declared width, `None` only where the result is undefined or the
  type is a float. Its `mask` / `all_ones` / `complement` / `ones_at` /
  `le_bytes` hold an unbounded `UBig` to a declared width, and `signed` /
  `unsigned` read and write that width's two's complement.
- `IntBinaryOp::is_shift`: whether operand 1 is a shift count.
- `ExpectedValueKind::admits`, the slot-kind check `validate`, the template
  and the builders read.
- `NodeKind::{expected_input_kind, input_arity, operand_width}`,
  `OperandWidth` and `Conversion`: a node's input arity, and each input
  slot's kind and width, from the node signature. `validate` reads the
  width, the template all three, the builders the kind.
- `IRViewer::access_address` and the `NodeKind::*_SLOT` input-slot constants.
- `ValueKind` implements `Display`: a typed value prints as its type (`f64`),
  as the builder and template signature errors print it.
- `vn_container::{vn_end, vn_overlaps}`, the exact varnode end and overlap
  `strider-target` and the lifter's p-code folder now share.
- `strider_cfg::DecodeEngine`: the Sleigh and the decode context it carries
  across functions; `DecodeEngine::new(arch, sleigh)?.build_cfg(entry, &opts)`
  builds a CFG as `Lifter::build_cfg` does.
- `Cfg::region_containing`: the region owning an address, of those containing
  it the one with the greatest start.
- `strider_cfg::user_op_of`: the user-op a `CallOther` names, the one decode
  the CFG and the lift read.
- `strider_cfg::direct_call_target`: a direct `Call`'s target address, read
  only from a code-space operand; the one rule the CFG and the lift select by.
- `Dominators::intervals` / `Dominators::tree_preorder` and
  `DomIntervals::{span, preorder}`: the dominator tree's one numbering; its
  `preorder` takes siblings in discovery order, `tree_preorder` ascending by
  vertex.
- `FunctionBuilder::record_register_arg_carriers` outside `test-util`.
- `elf::RelocationMode` (`Unapplied`, `Applied { got_is_rom }`),
  `ElfFileMemReader::new(&OwnedElf, elf::RelocationMode)` and
  `ElfFileMemReader::check_unchanged`.
- `elf::AddressSet`, the merged range set `AddressSet::merged` builds (an
  empty or inverted range is dropped), read through `AddressSet::as_slice`;
  `AddressSet::covers` answers whether it holds a whole range in O(log n).
- `OwnedElf::image(source, mode)` returns an `elf::ElfImage`: the fetch and ROM
  region sets and the writable `AddressSet` a `RelocationMode` picks, from one
  parse. Each set keeps one region per start: where a writable mapping cuts
  two overlapping read-only ones, the pieces left that share a start collapse
  to the later, as `MemRegionsLookupTable` collapses them.
  `RelocationMode::Applied { got_is_rom }` also builds a relocated
  `ElfFileMemReader`.
- `float_unary(op, x)` and `template::float_unary`, the op-valued float
  unary builder beside `float_binary`.
- `strider_target::ArchNames`, an arch's register and space names, default
  code space and `ram` address width, which is all a render reads;
  `SleighArch::names()` builds it once per preset per process.
  `ArchPreset::name()` is the preset's constructor name.
- `strider_target::calling_convention::{vn_for_name, regs_to_vns}` are
  public, the latter over any iterator of names.
- Test-only counters: `strider_graph::edge_reads()` behind the new
  `edge-read-count` feature, and `strider_ir::EditFunction::uses_moved()`
  behind `test-util`. The scaling tests count edges read, uses moved, bytes
  allocated and tree descents instead of timing a ratio.
- `graph_algorithms::walk::acyclic_postorder(graph, roots)`: the post-order
  of what the roots reach, every node after its successors, or `Err(node)`
  for a node with a successor on the DFS path, which is on a cycle.
- `NodeKind::{CTRL_SLOT, CTRL_MEM_SLOT, ACCESS_MEM_SLOT,
  INDIRECT_BRANCH_TARGET_SLOT, SWITCH_SELECTOR_SLOT}`: the input slots of the
  control predecessor, the memory token after it, a `Load` / `Store`'s memory
  token, an `IndirectBranch`'s target and a `Switch`'s selector.
- `strider_pattern::node_builders::defer_drop`, the drop pit that frees a
  deep operand tower without recursing.
- `matcher::InputSlot { At, Any }` and `MatcherBuilder::input_any(node,
  prod)`, the raw existential input.
- `matcher::KindSpec` implements `PartialEq` and `Eq`.
- `strider_ir::AssumptionOptions`, still re-exported as
  `strider_opt::AssumptionOptions`: `union_with` adds every claim another
  set holds, `omitted_by` names the claims another set lacks, `holds_all`
  answers whether it holds every claim of another, and it derives
  `PartialEq` / `Eq`.
- `Lifter::arch`, the arch the engine decodes with; `Strider::arch` reads
  it.
- `NodeKind::PHI_TOKEN_SLOT`, a phi's token input, and the output slots
  `CTRL_OUT_SLOT`, `CTRL_MEM_OUT_SLOT`, `MEM_OUT_SLOT`,
  `REGION_PHI_OUT_SLOT`, `IF_TRUE_OUT_SLOT` and `IF_FALSE_OUT_SLOT`;
  `NodeKind::output_head_len` is a `Call` / `CallOther`'s first value output.
- `Function::is_entry_sp`: whether a value is the entry stack pointer.
- `BuiltCallingConvention::float_arg_prefix`, the float arguments a call
  passes: `float_arg_slots` up to the first untracked one.
- `Endianness::slice_shift_bits`, a register slice's bit distance from its
  container's LSB.

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
- `load_elf` refuses an ELFCLASS32 MIPS image whose ABI is n32, EABI32, EABI64
  or O64, none of which has a preset; it assumed O32 for all of them and read
  arguments past the fourth out of the wrong registers.
- `CfgOptions.known_targets` reads back target lists as tuples, so the
  read-only view is read-only all the way down: editing one of those lists was
  a silent no-op against the table `analyze` reads.
- `LifterOptions.per_address_ccs` reads back as the same read-only
  `mappingproxy` on every access, as the `CfgOptions` tables do; it built a
  fresh dict of fresh objects per read.

Analysis:

- The three unchecked memory claims move off `LifterOptions` into
  `LifterOptions(assumptions=AssumptionOptions(...))`, and
  `strider.lift.AliasMode` is gone; `LifterOptions(alias_mode=)`,
  `calls_clobber=` and `assume_distinct_sp_bases_disjoint=` raise `TypeError`.
  `alias_mode` is `stack_global_disjoint` (default `True`), `calls_clobber` is
  `assume_incoming_args_survive_calls` (inverted, default `True`) and
  `assume_distinct_sp_bases_disjoint` is `distinct_sp_bases_disjoint`, beside
  the new `callee_preserves_stack_args`, `noalias_allocators`,
  `escape_analysis` and `return_slots_survive_untracked_writes` (default
  `True`, the claim return verification rests on).
  `AssumptionOptions.none()` clears all seven.
- `build_cfg` raises a `StriderError` naming the cause for an `entry` it
  cannot use, the way `analyze` does; it raised a bare `OverflowError`.
- A `Lifter` decodes only on the thread that built it. `analyze`, `build_cfg`,
  `optimize`, `pcode_at`, `reg`, `reg_name`, `call_other_abi`,
  `user_op_names` or a renderer given `lifter=` raise `StriderError` on any
  other thread. The handle moves and drops anywhere; build a second one over
  the same `arch` / `reader()` / `rom()` to work off-thread, and call its
  `analyze(addr, cc)`, a plain handle carrying no symbols and no default
  convention.
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
- Building the wheel or the sdist needs maturin 1.10 or newer: `license` is a
  PEP 639 SPDX expression and `license-files` names several files, which 1.8.1
  and earlier refuse to parse and 1.8.2 through 1.9.x silently truncate to the
  first entry.
- The fixture binaries are Git LFS objects; a fresh clone needs
  `git lfs install && git lfs pull`.
- Every pattern builder takes the Python name: the `int_` and `any_` renames
  above (`bit_not` -> `int_not`, `truncate` -> `int_truncate`, ...), `any` ->
  `anything`, `if_node` -> `if_else`, `CallPat::at_any(addrs)` ->
  `target(int_const(addrs))`, `SwitchPat::address` -> `selector`,
  `signed_int_const` -> `int_const_any_width`, `preceded_by` -> `ctrl`,
  `mem_in` -> `mem`,
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
- `MatcherBuilder::constrain_input_widths` is gone; pin a width on the one
  vertex that must carry it with `MatcherBuilder::set_value_width`.
- `NodeKind::Switch(SwitchTableId)` and `NodeKind::Call { cc: Option<CcId> }`
  carry their case table and override convention. `SideTables::{switch_targets,
  set_call_cc, call_other_name, set_call_other_name}` moved to `Function`;
  `set_call_other_name` takes the `user_op_id`. `set_switch_targets` is gone:
  mint a table with `Function::add_switch_table(targets) -> SwitchTableId` and
  build `NodeKind::Switch(id)`.
- `NodeKind::SegmentOp`, `IRBuilderExt::build_segment_op` and the `Seg` / `Off`
  slot roles are gone; a `SegmentOp` or `Indirect` opcode fails the lift by
  name.
- `IndirectBranch` and `Switch` take an optional trailing input, the ISA mode
  their instruction commits (`build_switch_with_mode`,
  `IRViewer::switch_isa_mode`). `Unreachable` takes an optional memory input.
  `build_switch` returns the `NodeId` it created.
- `ValueType` gains `I24`, `I40`, `I56`, `I72`, `I96`, `I112`, `F16` and `F128`.
- `ValidationError` gains thirteen variants and the enum is not
  `non_exhaustive`, so an exhaustive match stops compiling:
  `ArithmeticWidthMismatch`, `DanglingCcId`, `DanglingInitialVnId`,
  `DanglingSwitchTableId`, `DataCycle`, `FloatConstUnrepresentableType`,
  `FloatConstWidthMismatch`, `InitialVarTypeMismatch`, `InputNotAvailable`,
  `LostStore`, `MemoryRewind`, `NoTerminatorReachable` and
  `UnmergedMemoryEffect`. `FunctionBuilder::build` fails with a
  `ValidationReport` whose `errors` field holds them.
- `strider_ir::dominates`, `control_edge_dominators` and `control_dominators`
  are gone, and with the last of them strider-ir's production dependency on
  petgraph: `control_dominator_tree` / `control_edge_dominator_tree` return a
  `DominatorTree`, whose `dominates` is O(1), and petgraph's `simple_fast`
  stays only as the tests' oracle.
- `strider_ir` renames the stack side tables for the memory classes they cover:
  `SpDecomp` -> `MemDecomp`, `StackId` -> `MemoryId`, `stack_slot` ->
  `memory_class`, `stack_slot_resolved` -> `memory_slot_resolved`,
  `set_stack_slot_not` -> `set_not_memory`, `clear_stack_slots` ->
  `clear_memory_slots`.
- `Graph::retain_reachable` is `retain_reachable_stale_cache`, and
  `Graph::update_input` panics on a `UseId` its node no longer holds.
  `Graph::remove_node_input` needs the `test-injectors` feature and
  `FunctionBuilder::build_branch` the `test-util` feature.
- `FunctionBuilder::build_call_other_abi` is gone with no public replacement;
  `strider-lift` builds an ABI-carrying `CallOther` through its own
  crate-private path.
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
  `DotStyle::dark()`, `dark_cfg()` or `empty()`.
- `dot::DotEmitter::node` doubles every backslash in a label, so a `\l` or
  `\n` renders literally; `node_raw_label` is the one that keeps line breaks.
  `dot::GraphDot::as_dot_with_state` is gone (`as_dot` remains): the state it
  handed back only maps nodes under `cfg(test)`, so a production caller got
  nothing usable.
- `CfgOptions` gains public `call_other_overrides` and `data_ranges` fields,
  so a struct literal needs `..Default::default()`.
- `strider_cfg::CfgOptions<'a>` and `strider_lift::LiftOptions<'a>` carry a
  lifetime: `CfgOptions::known_targets` is a `&'a FxHashMap<PcodeInsnAddr,
  ResolvedTargets>` and `LiftOptions::per_address_ccs` a `&'a FxHashMap<u64,
  BuiltCallingConvention>`. Both took their map by value, so every `analyze`
  with a seeded table copied the whole table twice. A struct literal must name
  a borrow (`known_targets: &known`), and `Default` borrows one shared empty
  table instead of allocating. `strider_cfg::Builder::for_arch` takes
  `&'a CfgOptions<'a>` and `with_per_address_ccs` a `&'a FxHashMap<..>`, and
  code forwarding `LiftOptions::per_address_ccs` to `Lifter::build_cfg` passes
  it as it stands rather than by reference.
- `strider_cfg::ResolvedTargets` carries `ResolvedTarget { addr, isa_bit }`, so
  a `known_targets` map needs `ResolvedTarget::from(addr)`.
- `AnalyzeResult` gains `unverified_seeded_sites`, `interior_branch_targets`,
  `isa_mode_conflicts`, `unmapped_branch_targets` and
  `undecodable_branch_targets`; a struct literal must name them.
- `AliasMode`, `MemAliasOptions` and `OptOptions::arg_alias` are gone.
  `OptOptions` is `{ resolve_indirect_branches, assumptions }`, with the
  claims in `AssumptionOptions` as listed under Breaking, Python;
  `AssumptionOptions::default()` keeps three on and `::none()` clears all
  seven.
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
- A `Call`'s flat argument list is longer on the userland PowerPC presets,
  which now name `v2`-`v13` for vector arguments, so a `call().arg(n)` query
  reads a different operand on both surfaces: a collected stack argument's
  first position is `8+25` on PPC64 and `8+20` on PPC32, where it was `8+13`
  and `8+8`. The three `*_linux_kernel` flavours name no vector register, so
  nothing moves there.
- `SleighArch::probe_regs` needs the `test-util` feature.
- The ARM processor-mode `CallOther` rows apply to the ARM32 presets only, and
  a processor-mode switch (`setUserMode`, `setFIQMode`, ...) clobbers
  `r8`-`r12` as well as `sp` and `lr`. `setStackMode` is its own row and writes
  `sp` alone; `setThreadMode` stays unclassified.
- `OwnedElf::file` is gone; use `checked_file`.
  `elf_get_loadable_regions_including_writable`, `elf_load_with_relocations`,
  `elf_load_readonly_with_relocations`, the two sections-only region loaders,
  `apply_elf_relocations` and `apply_elf_relocations_autoload` are gone; use
  `OwnedElf::regions(source, filter, relocate)`. `MemRegion::data` /
  `data_mut` are gone; a region serves bytes through `read`.
- `ElfFileMemReader::from_bytes` and `::from_path` are gone, and
  `elf::relocations` is private.
  `::from_object(&obj)` is the one constructor and it copies each mapping, so
  the reader has nothing to stat: `from_bytes(b)` is `object::File::parse(b)`
  then `::from_object(&obj)`, and `from_path(p)` is `load_elf(p)` then
  `::from_object(owned.checked_file()?)`. A zero-copy view of a mapped ELF is
  an `OwnedElf::regions` load behind a `MemRegionsLookupTable`.
- `OwnedElf::function_entry` is gone; build an `OpdTable` over a
  `checked_file` instead, which is what every caller already did.
- `elf::LoadFilter` gains `ImmutableAndRelocatedGot`, so a match over it stops
  compiling. `OwnedElf::regions` errors for that filter with `relocate=false`:
  the filter claims the loader has relocated `.got` / `.data.rel.ro`, which
  `relocate=false` denies.
- `BuiltCallingConvention` gains `entry_address_vn`, the register an ABI
  guarantees holds the function's own entry address on entry to it (PPC64
  ELFv2 `r12`, MIPS abicalls `t9`), which the lift binds to the entry address;
  a struct literal must name it.
- `BuiltCallingConvention::ret_and_clobber_vns` reports REGISTER clobbers only,
  so a `Call` no longer carries an output per tracked Sleigh temporary.
- `strider_cfg`'s region types carry resolved targets and extents, all fields
  public and none `non_exhaustive`: `RegionTerminator::TailCall`'s and
  `Switch`'s `target`/`targets` are `ResolvedTarget`s, `Switch` gains `addr`,
  `Region` gains `empty_span_len` and `RegionInstruction` gains `len`.
- `OptimizerPipeline::add` and `add_post_pass` require `Send`, and `passes()` /
  `post_passes()` hand back `Box<dyn ... + Send>`, so a pass capturing an `Rc`
  no longer compiles.
- `LiftOutcome` gains `switch_anchors` and `return_sites`, so a struct literal
  must name them.
- `entity_utils::UnionDag`'s value parameter is `V: Copy + Eq + Hash`.
- `strider_pattern::IndirectBranchPat::isa_mode` is gone; the slot is
  `.input(3, p)`, which the struct's slot layout names.
- `ArchPreset::arch` needs the `test-util` feature. A caller holding a preset
  names the `SleighArch` constructor.
- `Lifter::arch` is gone; `Strider::arch` remains.

### Added, Python

- `load_elf(relocated_got_is_rom=True)` / `add_elf(relocated_got_is_rom=True)`
  serve `.got` and `.data.rel.ro` as read-only memory, so a load through a GOT
  slot folds to the address it holds. Off by default: it is a claim about the
  runtime that the image cannot prove. Only a section of those names the loader
  really maps writable is served; `.got.plt` never is, lazy binding rewriting
  those slots. The flag with `apply_relocations=False` is refused: the claim is
  that the loader has relocated those slots, which the other flag denies.
- `LifterOptions(assumptions=AssumptionOptions(...))` tunes memory precision;
  [docs/python-api.md](docs/python-api.md#2-analyzing-a-function) says what
  each claim buys. `AssumptionOptions.none()` makes none of them.
- `cfg.is_complete()` and six incompleteness channels:
  `unresolved`, `cfg.unverified_seeded_sites()` (answers exactly the caller's
  `known_targets`, sites consumed as a tail call, returns the caller seeded,
  and an arm seated on a mode nobody evaluated),
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
- `CallingConvention.powerpc_sysv32_linux_kernel()`,
  `powerpc64_elf_v1_linux_kernel()` and `powerpc64_elf_v2_linux_kernel()`, the
  kernel-internal PowerPC flavours, which name no vector register.
  `load_elf` picks one when `e_entry` is at or above `KERNELBASE`
  (`0xc000_0000` on ppc32, `0xc000_0000_0000_0000` on ppc64), independently of
  the ELFv1/ELFv2 choice. x86's kernel ABI has no header bit to read and still
  needs an explicit `cc=`.
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
- `Function.to_text()`: the IR as canonical text, a header (endianness, tracked
  varnodes, default convention, argument carriers) and then one line per node,
  stable across runs.
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
- `Cfg::isa_mode_conflicts` / `interior_branch_targets` /
  `unmapped_branch_targets` / `undecodable_branch_targets` /
  `undecodable_seeded_targets` / `link_register_seated` / `tail_call_seated` /
  `function_isa_bit` / `flowing_isa_bit_at_site` / `space_ids` /
  `switch_arm_regions`, and `AnalyzeResult::is_complete`
  (`unverified_seeded_sites` is an `AnalyzeResult` field, listed above).
- `CfgOptions::data_ranges` (`DataRanges`) and
  `strider_reader::elf::mapping_symbol_data_ranges`.
- `strider_cfg::Builder::with_user_op_names` and `with_transient_defaults`;
  `SleighArch::has_delay_slots` and `internal_registers`;
  `FunctionBuilder::set_internal_registers`.
- `LiftOutcome::return_sites` and `strider_opt::ReturnTargets`.
- `OptimizerPipeline::has_post_pass`.
- `ElfSectionLayout::extern_address`, `OwnedElf::writable_ranges`,
  `elf::without_ranges`, `OwnedElf::checked_file`.
- `elf::LoadFilter::ImmutableAndRelocatedGot`, the immutable image plus `.got`
  and `.data.rel.ro`.
- `graph_algorithms::dominance::dominators` (Lengauer-Tarjan), and
  `strider_ir::DominatorTree` with its `span` built over it.
- `NodeKind::input_head_len`, `expected_output_kind` and `is_terminator`.
- `ArchPreset::ALL`.
- `BuiltCallingConvention::float_arg_slots`.
- `strider_ir::node::low_bits_mask_u128`.
- `strider_cfg::{FlowContext, FlowVars, UndecodableTarget}` and
  `Builder::with_flow_context`.
- `strider_reader::elf::{RegionSource, LoadFilter, ElfSectionLayout, OpdTable}`.
- `ValueType::ALL`, `Strider::{user_op_names, rom, arch}`,
  `dot::DotEmitter::node_raw_label` and `strider_opt::apply_rules_count_with`.
- `SleighArch::callother_output_is_exact_in`: whether a `CallOther` output
  narrower than its tracked container leaves the rest of that container alone,
  true on x86 and x86-64 outside the x87 register file.
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
- The indirect-branch resolution loop is one round per level of discovery.
  The partial-function lookahead that seated sites between rounds is gone: the
  ordinary rounds seat what it did, so the result is unchanged.
- `validate` rejects IR it used to accept: a non-phi node that is its own
  transitive producer; the memory, `InitialVar` and `LostStore` checks above; an
  integer operation mixing operand widths; a `CcId` or `SwitchTableId` no
  interner holds; an input not available at its use; a reachable graph with no
  terminator; and a `Region` that drops an arm's memory effect, which is
  `UnmergedMemoryEffect` rather than the `MemoryRewind` whose message describes
  a reader taking a stale state.
- `strider-opt` no longer depends on petgraph either, no public item of its
  own changing: the jump-table index classifier climbs the `Dominators`
  `graph_algorithms::dominance::dominators` builds, and `mem_ssa::layout`
  orders its memory graph with
  `graph_algorithms::dominance::immediate_dominators` and `DomIntervals`.

### Performance

- A `Call` carries an output per clobbered REGISTER rather than per tracked
  Sleigh varnode, a p-code `CALL` writing no UNIQUE varnode: over the 2,718
  fixture functions the lift drops 26,432 `Call` outputs, none of them used,
  and on the 24 largest x86-64 functions of a release binary the lifted graph
  loses 26% of its nodes and 49% of its values, `build_ir` running 1.31x
  faster and `analyze` 1.24x.
- A Sleigh temporary gets a phi only where one is live across a region
  boundary, which is only where one machine instruction's p-code is split; it
  was written in almost every region and so given a phi at almost every join.
  On those same functions the graph loses a further 21% of its nodes and 16%
  of its values, `build_ir` runs 1.16x faster and `analyze` 1.09x. The
  optimised text is unchanged on every fixture function either way.
- A `per_address_ccs` table costs what the function calls rather than its own
  size: the builder borrows it and the lift seeds only the overrides this
  CFG's call targets select (10,000 entries over `mov eax,1; ret`: `build_cfg`
  3.24 ms to 9.8 us, `build_ir_with` 561 us to 16 us, both now flat in the
  table size). From Python each `analyze` still resolves every entry's ABI
  against the register table, which 10,000 entries cost 5.4 ms.
- A pattern query no longer doubles per nested failing branch pattern (a
  40-`If` graph with no match: 14.6 s to 0.001 s), and a commutative node whose
  two inputs are one value tries one operand order.
- `find_all` attempts a root once per distinct anchor, stops at the first
  operand ordering of a pattern with no visible capture, and keys each join
  level on what each prefix position binds rather than on the intersection,
  which left a chain join running the whole cartesian product.
- `LoadForward`, `FunctionArgDetect` and `CallStackArgCollect` answer
  nearest-clobber queries by climbing a memory dominator tree instead of
  walking the memory graph per query (debug build: `LoadForward` over 1600
  call diamonds 7.9 s to 0.10 s). An irreducible memory graph has no such tree,
  and there the walk now publishes each location's start-independent answers
  for the rest of the climb (clang -O0 goto-into-loop inputs, `LoadForward`'s
  share of `analyze` at 500 / 1000 / 2000 / 4000 statements: 0.117 / 0.466 /
  2.787 / 9.994 s to 0.013 / 0.035 / 0.089 / 0.225 s).
- Dominators come from Lengauer-Tarjan rather than petgraph's quadratic
  iterative scheme; join `dominates` constraints, `phi_input_from_edge` and
  value-range guard lookups answer from dominator-tree intervals.
- `Function::validate` is linear (192k nodes: 2.1 s to 232 ms), and so is
  draining a `Worklist` and dead-branch elimination over chained constant
  branches.
- Region lookup is O(log n) for full and partial reads.
- A run of `$d` data mapping symbols is one range: each mark scanned the rest
  of its section for the next code mark, so k consecutive marks cost O(k^2) and
  produced k nested ranges.
- One def-use walk order serves every read at a live-graph reading, where each
  peephole pass seeded from its own whole-graph post-order and so re-walked the
  graph once per pass of a pipeline iteration.
- Compaction sizes its arenas off the survivors rather than doubling beside the
  graph it replaces, and a finished CFG region drops its instruction vector's
  push slack.
- Resurrecting a value costs its own cone rather than an allocation spanning
  the node arena, and a phi's incoming edge is an indexed read of its
  `Region`'s inputs rather than a walk of the use list, which cost O(k) per arm
  at a k-predecessor join.
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
  known targets: 32.0 ms to 1.6 ms), and `LifterOptions.per_address_ccs` is
  served the same way.
- A seeded `known_targets` table costs an `analyze` nothing: the option tables
  are borrowed rather than owned, `PyCfgOptions` seats the table into the CFG
  builder's own form at construction and lends it per call, and the resolve
  loop copies it only where it seats a target (20,000 entries: 0.95 ms an
  `analyze` to nothing).
- `pcode_at` keeps what each sweep decoded, so a walk over a function's
  instructions decodes it once instead of re-sweeping from the entry per
  address.
- A CFG build reuses the `Lifter`'s Sleigh user-op table instead of fetching it
  over FFI, and `Function.to_dot(pretty=True)` resolves the register table once
  per render.
- `UnionDag::for_each` costs the size of the answer, not of the arena, so
  reading `asm_fingerprint` over a large function is linear.

### Fixed

Lifting and memory:

- PowerPC `mtfsf` / `mtfsfi` (the Power ISA 2.06 `L=1` / `W=1` forms, on the
  ppc64 presets) and
  `wrtee` / `wrteei` declare the FPSCR and MSR registers they write, which the
  sla reads back itself; a read either side of one folded to the same value.
  The x86 `XSAVE` / `XRSTOR` footprint gained the opmask, high ZMM, MPX and
  CET-supervisor state it moves.
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
- A call through an x86 trap vector (`int imm8`, `into`) pops no return
  address, where SP drifted by 4 or 8 after every syscall. Only a `swi` trap
  vector: the rule keyed on the target's producer being any `CallOther`, so
  `rdrand rax; call rax` lost the callee's return-address pop.
- A `CallOther` output narrower than its tracked container writes that slice
  alone on x86 and x86-64, where the SDM leaves the rest of the container as it
  was: the output was widened to the container and written whole, so
  `mov eax,0x12345678; in ax,dx; shr eax,16; ret` returned a shift of the
  intrinsic's opaque result instead of `0x1234`. Decided per register file:
  every other architecture redefines those bytes without the sla saying so, and
  so does an x86 MMX write to its enclosing 10-byte x87 `ST`, so both keep the
  whole-container clobber.
- A calling convention names the register holding the function's own entry
  address (PPC64 ELFv2 `r12`, MIPS abicalls `t9`) and the lift binds it, so
  TOC-relative addressing folds: four ppc64 fixture jump-table sites resolve
  where they deferred. ELFv1 names a descriptor word rather than a register and
  keeps none.
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
- The USERLAND PowerPC conventions pass vector arguments in `v2`-`v13` and
  return a vector in `v2` (ELFv2 a homogeneous vector aggregate in `v2`-`v9`),
  where no preset named a vector register at all and DCE deleted every
  AltiVec/VSX result and every caller's vector argument setup. An ELFv2
  `__float128` or IEEE `long double` return comes back in `v2` too. The three
  `*_linux_kernel` flavours name no part of it, PowerPC Linux being built
  `-mno-altivec -mno-vsx`, so a function that never touches a vector seeds no
  16-byte varnode. The vector window follows the float one, which moves the
  stack-argument positions listed under Breaking, Rust.
- `BuiltCallingConvention::validate` rejects a stack-pointer SUB-register in a
  register list, the disjointness rule being by byte range; `ESP` beside `RSP`
  was accepted and made the stack pointer `arg 0`.
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

- IR validation runs its dataflow checks even when a node is missing an asm
  fingerprint, so an error bundle can carry both.
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
- `PhiCollapse` and `RegionCollapse` skip a region unreachable from the entry,
  whose only predecessor is its own `If`: collapsing one built a value from
  itself, which `validate` rejects.
- A known-bits fold attributes its folded cone to the node it builds, so the
  asm fingerprint of the replacement names the instructions it stands for.
- `range_of` resolves a chain of phis under a depth budget rather than giving
  up at the first one.
- Return verification locates a RAM store's address through the same guess
  machinery a load's address goes through, so a write through a frame pointer
  reloaded off the stack is checked against the saved slot. What is left, a
  store at no locatable stack address and any callee or user-op, is the claim
  `AssumptionOptions::return_slots_survive_untracked_writes` carries. A guess
  is not a placement, so it bears on the answer only with that claim cleared:
  the guess picks one phi arm, and a prologue's own `push lr` through a merged
  SP otherwise read as a write to the return slot, which reported a genuine
  return.

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
- A narrowing mask on a table cell is not read as the index: `(tbl[i] & m) +
  base` put the cell's range below its load width, which made the cell the
  shallowest bounded candidate and seated one arm per masked value. The index
  is the bounded value below the cell's load and inside its address; a cell the
  evaluator cannot fold keeps the masked value, the only bound a `*p & m`
  dispatch has.
- A switch over a negative case base, which guards `index + k` that
  `ConstantFold` then merges into the table base, takes its bound from the
  tightest guarded sibling with the index pinned `k` lower, `Add` being modular
  at one width; the index itself read as unbounded.
- A single-register pop into the PC (`ldr pc,[sp],#4`) is classified as a
  return rather than left unresolved: with a call between the push and the pop
  nothing folds the stack load back to the link register. `ReturnTargets`
  separates it from the `push X; pop pc` tail call it looks like, and the
  seated `Return` records a return site, so every seat is re-checked against
  each round's IR.
- A return a CALLER seeded is reported on `unverified_seeded_sites` alone; it
  was also reported as an unverified return, which the caller had asserted.
- A `Return` the CLASSIFIER seated rides no channel once
  `return_slots_survive_untracked_writes` carries its proof, so an ARM
  `push {lr}; bl; pop {pc}` comes out complete with every channel empty.
  Clearing that claim and re-running is how a caller audits one: the same site
  reports with the claim off. A caller-seeded `Return` whose re-check FAILS is
  reported on `unverified_seeded_sites`, not as an unresolved dispatch, because
  the seat decided a CFG edge.
- A region starting mid-machine-instruction keeps the decode context those
  bytes were lifted in: the region-start reset re-decoded the instruction the
  region continues and spliced a second constructor's p-code onto the half
  already in the graph, so `gcc -march=armv4t`'s `movne lr,pc; bxne r3` lost
  its call. A lock-prefix re-entry is treated as exact only where the region
  starts at that instruction.
- A seeded arm that fails to decode names the bytes that would not decode on
  `undecodable_branch_targets`; the report carried the arm, which decodes, so
  the failing address reached no channel. Refusing the arms that reach such
  bytes stops at the next dispatch, so an arm reaching them only through
  another table keeps its edge.
- A run of zero-p-code-op instructions between a call and bytes that hold no
  instruction ends that call as no-return, and a branch into such a run keeps
  its successor.
- `unmapped_branch_targets` and `undecodable_branch_targets` are sorted and
  deduplicated.
- A decode that runs off the end of the image mid-instruction ends the region.
- A branch into a region's last instruction, and a region split, keep every
  edge: the build aborted, decoded an overlapping region or dropped a
  successor.
- `Cfg.region_at` finds an address inside a region's last instruction.
- A reused engine decodes a function the same as a fresh one: ARM's
  `mov lr, pc` state and MIPS's `PAIR_INSTRUCTION_FLAG` and MIPS16e
  `ext_delay` no longer carry into the next decode.
- A reused engine also re-derives the ARM context an instruction commits at a
  LATER address, which outlived the `build_cfg` that produced it: the IT-block
  state (`condit`) and the ThumbEE mode (`TEEMode`). Both are re-derived from
  this build's own decode, so the bytes after an `IT` are predicated only where
  this function's own `IT` predicates them, and a ThumbEE region decodes as
  ThumbEE only where this function's own `enterx` entered it. Only the ARM
  presets commit such a value: x86 and AArch64 commit nothing, MIPS commits the
  ISA mode only at a `jalx` target analysis never decodes, and PowerPC's
  `linkreg` selects constructors that emit identical p-code. An instruction
  lifting to NO p-code ops (ARM `enterx`) now records its own commit too, where
  the fallback used to overwrite it.
- Two edges reaching one address with different IT state remains
  first-decode-wins and unreported, which ARM calls UNPREDICTABLE.
- `Cfg.isa_mode_conflicts()` is sorted and deduplicated, so an address
  clashing on several edges appears once rather than once per arrival.
- An `analyze` given a post-pass named `IndirectBranchClassify` still runs the
  real classifier.
- Three inputs that crashed `build_cfg` raise `StriderError`: a malformed
  instruction in a MIPS delay slot, an AArch64 operand with a null address
  space, and a region at the top of the address space.

Patterns:

- A commutative node with one pinned operand tries both orders, and a capture
  on an operand's sibling output keeps its binding.
- A match's identity is its visible bindings, so a commutative swap that moved
  two fanned-out sub-patterns between operands no longer reports the same match
  twice (`bool_and(float_is_nan(_), float_is_nan(_))` gave two hits on one
  root).
- A `first_of` cut is scoped to the root attempt, and keyed on the IR node the
  alternation was offered, rather than to one invocation of it. Scoped per
  invocation, the two mirror spellings of a commutative query answered
  differently; keyed on the pattern node alone, a commutative ordering or an
  `any_input` slot re-offering the alternation a DIFFERENT node cut the arm
  holding the match.
- `inputs_of_width` checks the matched node's own inputs. Pushing the width
  onto the input producers landed it on an alternation's ARMS and on the far
  side of a cast walked through under `ignore_casts`, so
  `inputs_of_width(64, one_of![int_eq(..)])` demanded a 64-bit `I1` compare and
  matched nothing.
- A refusal an unmatchable `if_else` branch pattern left behind is dropped with
  that branch, so rebuilding the query no longer errors.
- The node cap counts a pattern tree-unfolded, charging a shared vertex once
  per consumer: the engine walks a vertex's producer once per slot reaching it,
  so a sub-pattern feeding both operands of a chain of adds cost 2^depth walks
  and stack frames, and a 12-node pattern overflowed a 2 MiB thread instead of
  being refused. An alternation's inputs are alternatives, run one at a time
  with the frame unwound between them, so it is charged its DEEPEST arm rather
  than their sum, which refused patterns the engine runs.
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

- A PT_LOAD segment declaring more file than it maps (`p_filesz > p_memsz`)
  serves only the bytes it maps. The tail belonged to no mapping yet was
  readable as program memory, and on a writable segment the read-only view
  answered for it, since permissions are tracked on `p_memsz`.
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
- An instruction relocation field is read and written in the INSTRUCTION byte
  order, which on `aarch64_be` is little-endian whatever `EI_DATA` says: every
  classified A64 field was byte-reversed, turning an `adrp` into an `adr`, an
  `ldr` into a store and a `bl` into undecodable bytes.
- An `SHT_REL` `R_*_RELATIVE` / `R_MIPS_REL32` with no symbol is served as
  assembled, its field already holding the whole value with the image base
  modelled as 0; it fell through to a hole, so a read-only pointer table
  served nothing on an image with text relocations.
- An undefined `STB_WEAK` symbol's synthetic address reaches only a call or
  jump displacement, which reveals no address. Such a symbol resolves to 0 at
  link or module-load time as readily as to an address, so handing every site
  the address folded `if (&w)` to true and dropped the null path; its GOT slot
  is left unmapped and a field that would hold the address outright is left
  uncomputed.
- A ppc64 ELFv1 `ET_REL` object's function symbols resolve through the
  relocations on their `.opd` entry words, an unlinked `.opd` holding zeros, so
  `analyze` by name no longer decodes descriptor bytes.
- x86-64 `R_X86_64_GOT64` and `R_X86_64_PLTOFF64` resolve, so
  `-mcmodel=large -fPIC` code addresses its globals rather than reading a hole.
- Walking the dynamic table depends on `.dynsym` being present rather than on
  the image being dynamically linked, and a Python `read` returns `None` inside
  an uncomputed relocation's field as well as off the end of a mapping.

Python and the explorer:

- A mapping argument (`known_targets`, `call_other_abis`, `per_address_ccs`,
  `add_symbols`) is snapshotted before its elements are converted. A `dict`
  mutated during conversion, which a key's or value's `__index__` can do,
  raised `PanicException`, a `BaseException` that `except Exception` does not
  catch. `per_address_ccs` now also accepts any mapping, so the read-only
  proxy its getter returns round-trips through the constructor.
- `analyze` releases the GIL on a custom pipeline too, and `OptimizerPipeline`
  is usable from any thread and survives being applied.
- A `Function` or `Cfg` moved to another thread no longer leaks its `Lifter`,
  and an off-thread render raises `StriderError` rather than
  `PanicException`.
- A `MemReader` callback that panics raises `StriderError`, and one that
  re-enters `analyze` no longer aborts the process.
- `Cfg.neighborhood_dot` raises on an out-of-range region id.
- `add_elf` and `add_symbol_file` take a `pathlib.Path`, as `load_elf` does;
  they handed the argument straight to Rust and raised a pyo3 `TypeError`.
- `field` and `code_ptr` cut to their specific arm, so every field no longer
  comes back twice under a permissive base (`one_of` is a union, and the bare
  arm also matched the `base + K` node itself), and a negative field offset
  reads back signed rather than as 2**64-8.
- An empty `CallOtherAbi.custom(...)` reprs as a `custom`, not as the `pure()`
  it is unequal to.
- `explore.shutdown` leaves a server it did not stop able to serve: `stopping`
  was set on every target before the wait could time out and never cleared on
  the give-up path, so a server reported as not stopped answered with headers
  and an empty body.
- The explorer enforces its `Host` check on every loopback bind, not just the
  three canonical spellings, so a server on 127.0.0.2 or an IPv4-mapped
  loopback address can no longer be read through DNS rebinding.
- The explorer's autocomplete and query evaluator offer one table of pattern
  builders plus `Capture`, instead of everything in `dir(strider.pattern)`:
  picking a type alias or protocol inserted e.g. `PatLike()` and produced a
  multi-screen extraction error. The search placeholder names `int_add`, not
  `add`.
- `build_cfg` reports only the `known_targets` seeds it seated, so a
  whole-binary answer table no longer makes every `is_complete()` False and
  name addresses the function does not dispatch from.
- `pcode_at` answers the same whatever order the calls came in: a reused sweep
  engine re-decoded an address a later instruction had already pinned a decode
  mode at (an ARM backward `blx`).
- `add_elf` does nothing when the image is already fully loaded byte for byte;
  it appended every region and symbol again.
  It also refuses off the handle's thread before merging anything, so a rebuild
  that cannot run no longer leaves `symbol` and `read` seeing an ELF the
  decoder does not.
- A `custom(...)` `CallingConvention` or `CallOtherAbi` is checked against the
  register table it froze its varnodes against, not the preset name, so one
  built from `arm` works with an `arm_thumb` handle.
- `Function.rewrite` / `rewrite_all` compile the replacement first, so an
  invalid one no longer consumes a one-shot `into_pat()` `find` pattern.
- Importing strider no longer leaves `RUST_LIB_BACKTRACE=1` in the C
  environment every child process inherits.
- A pattern that shares one `Pat` into both operands (`q = int_add(q, q)`) is
  refused while it is lowered, instead of building its exponentially many
  matcher closures before the node cap can refuse it.
- DOT, HTML and p-code text are deterministic, and a backslash in a symbol name
  is escaped.
- `strider.explore.visualize(depth=-1)` raises `ValueError` rather than
  rendering the whole graph. `Lifter.visualize(depth=-1)` rejects it earlier,
  as the `OverflowError` of an unsigned argument.
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
- AArch64 scalar SIMD writes zero the rest of the vector register: 180
  constructors over 56 mnemonics (`fmadd`, `uaddlv`, `umaxv`, `fmaxv`, ...), as
  do `addv` into a byte destination and `frint32x` / `frint32z` / `frint64x` /
  `frint64z`. Every AdvSIMD and SVE write to a sub-register destination now
  clears what the machine clears, so no AArch64 constructor relies on the
  lifter widening a user op's output for it: the 64- and 128-bit vector writes,
  the SVE scalar reductions and extracts, the structure loads, and the
  W-register forms of the SVE counters, `ldapr` and `cas`. `srhadd` writes the
  register it names.
- AArch64 `usdot` / `sudot` / `bfdot` by element read the right lane, and
  `sudot` decodes.
- AArch64 `FRINT{A,I,M,N,P,X,Z}` lift through an opaque `NEON_frint` user-op
  instead of as a float-to-integer conversion, and the `FRINT32` / `FRINT64`
  forms through `NEON_frint32` / `NEON_frint64`.
- Thumb-2 `ands`, `bics`, `movs` and `mvns` set C from the shifter or the
  expanded immediate.
- ARM NEON `vmov.<dt> Dn[x], Rt` writes the element into its lane.
- The big-endian ARM VFP register file overlays `s` and `d` registers
  (`ARM7_be`, `ARM8_be`).
- ARM `vcmp` sets FPSCR's C when unordered.
- Thumb `sev` / `sev.w` raise `SendEvent`; A32 `csdb` no longer decodes as an
  `msr`, and decodes only on ARMv7 and later, where the encoding is defined.
- An ARM `vld4` / `vst4` single lane sits at the element size, Thumb
  `vst4.16` included.
- A Thumb-2 `0xEC8x`..`0xECFx` instruction no longer writes out of bounds in
  the parser.
- PowerPC `lhbrx` / `lwbrx` reverse the byte order on little-endian too, and
  32-bit PowerPC decodes `popcntw`.
- MIPS64 `clz` / `clo` count the word, `drotrv` rotates by the right amount,
  and `dsllv` / `dsrlv` / `dsrav` take six count bits.
- x86 `PSLLD` / `PSLLQ` / `PSRAD` read one count from `SRC[63:0]`, and
  `psrlw` / `psrld` read a memory count once.
- x86-64 32-bit destinations clear the upper half: `rdtsc`, `rdpmc`,
  `xgetbv`, `RDPKRU`, `LOOP` / `LOOPcc` under an address-size override, the
  `fsgsbase` and `rdpid` reads, `RDRAND`, `RDSEED`, `CMPXCHG8B`, 30 VEX and
  EVEX constructors, string instructions under a 0x67 prefix (RSI, RDI and the
  `rep` count), `LODSD`, `PEXTRW`, `MOVMSKPS` / `MOVMSKPD`, `MOVBE`, `LSL`,
  `SLDT`, `SMSW` and `PCMP[EI]STRI`.
- x86 `XADD` and `XCHG` with a 32-bit memory operand, `XSTORE`, `MONTMUL` and
  the `XSHA*` ops write their documented destinations, and MMX `PSRLW` /
  `PSRLD` / `PSRAD` with a register count latch that count once.
- x86 `extractps` and VEX `vpextrw` take their SDM destination forms, and the
  `fsgsbase` and `incssp` register operands require ModRM mod=3.
- x86-64 `rdfsbase`, `rdgsbase`, `rdpid`, `wrfsbase`, `wrgsbase`, `incssp` and
  `rdssp` honour `REX.B`; `rdsspd` decodes rather than as a reserved NOP, and
  `incssp` scales its byte count at the pointer width, where the multiply
  wrapped inside that byte.
- x86-64 `VMREAD` / `VMWRITE` take 64-bit operands in long mode, and `GETSEC`
  clears the upper halves of its results.
- A decode whose own context commit changes a value, an out-of-band context
  write, or a re-pinned context variable flushes the parse cache.
- A memory reader overstating its read is an error rather than a panic across
  the FFI boundary, a reader that panics is caught rather than unwinding out of
  the callback, and `setOutOfBandState` checks its operand index rather than
  reading uninitialised state. A region at the top of the address space no
  longer overflows the read callback.
- The Sleigh parse state bounds-checks the operand index, the descent depth
  and its own size, and throws rather than writing out of bounds on a deep
  constructor descent: PowerPC `vaddubm` and 126 VFP register lists no longer
  corrupt it.
- `Sleigh::clone` replays the pinned context the original held, so a cloned
  engine decodes as its source does, up to `MAX_CONTEXT_COMMITS` (4096)
  commits; past that the log is dropped and
  `Sleigh::clone_would_replay_context` says which of the two a clone gets.

## 0.1.0

First release.
